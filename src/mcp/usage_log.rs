//! The opt-in usage log: one privacy-safe JSON line per `tools/call`.
//!
//! `READING_MCP_USAGE_LOG=on` turns it on; unset or `off` writes nothing. Each line names the
//! tool, the outcome, the server-side latency and the response size, plus coarse shape facts
//! (document key without its checksum, the requested unit, the batch size). It never records
//! document text, search queries, source paths or URLs, or handle checksums — the record is a
//! fixed-field struct and a test fails if any other key appears in a line.
//!
//! Writes never slow a request: the dispatch hook hands the finished line to a bounded channel
//! and a background thread does the file I/O. A slow disk makes the channel fill, extra lines
//! are dropped, and the drop counter grows — no tool call ever blocks on the log.
//!
//! Rotation: daily files `usage-YYYYMMDD.jsonl` under `<state>/usage/`. Files older than the
//! retention window (`READING_MCP_USAGE_LOG_DAYS`, default 30) are deleted at startup and at the
//! first write of a new day.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError, sync_channel};
use std::time::SystemTime;

use serde::Serialize;

/// The default retention window, in days.
pub const DEFAULT_RETENTION_DAYS: u32 = 30;

/// Lines in flight at most; beyond that, lines drop rather than block a request.
const CHANNEL_CAPACITY: usize = 1024;

/// The exact key set of every usage line. The privacy test fails if a line carries a key
/// outside this set, and the record struct cannot add one without a compile error here.
pub const USAGE_LINE_KEYS: [&str; 9] = [
    "ts",
    "tool",
    "outcome",
    "retry",
    "ms",
    "resp_bytes",
    "doc",
    "unit",
    "batch_items",
];

/// One usage line. Every field is privacy-safe by construction: there is no field that could
/// carry text, a query, a path, a URL, or a handle checksum.
#[derive(Debug, Serialize)]
pub struct UsageRecord {
    /// UTC timestamp with millisecond precision, e.g. `2026-11-27T13:45:01.234Z`.
    pub ts: String,
    pub tool: String,
    /// `ok` or the error kind (`too_long`, `stale_handle`, `unknown_handle`, `ocr_required`,
    /// `too_large`, …).
    pub outcome: String,
    /// Only for `stale_handle`: whether the answer carried a concrete retry call.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry: Option<bool>,
    /// Server-side latency of the tool call.
    pub ms: u64,
    /// Size of the content returned, in bytes.
    pub resp_bytes: u64,
    /// The `d<key>` document part of the handle, without the checksum.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
    /// The requested unit, `sentence` or `paragraph`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    /// The item count of a batched read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub batch_items: Option<u64>,
}

/// The document part of a handle: `d<key>` without the checksum, or `None` when the argument
/// is no handle at all. Accepts the leading `b:`/`r:` cursor prefix and never copies anything
/// past the digits, so the checksum can never leak.
pub fn doc_key_of(handle: &str) -> Option<String> {
    let handle = handle
        .strip_prefix("b:")
        .or_else(|| handle.strip_prefix("r:"))
        .unwrap_or(handle);
    let rest = handle.strip_prefix('d')?;
    let digits: usize = rest.chars().take_while(char::is_ascii_digit).count();
    if digits == 0 {
        return None;
    }
    Some(format!("d{}", &rest[..digits]))
}

// ------------------------------------------------------------------------------------------------
// UTC calendar arithmetic (no dependencies): days ↔ civil date, ISO-8601 formatting.
// ------------------------------------------------------------------------------------------------

/// Days since the Unix epoch → (year, month, day), Howard Hinnant's `civil_from_days`.
fn civil_from_days(days: u64) -> (u64, u64, u64) {
    let z = days as i64 + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let year = if m <= 2 { y + 1 } else { y };
    (year as u64, m, d)
}

/// (year, month, day) → days since the Unix epoch, Hinnant's `days_from_civil`.
fn days_from_civil(year: u64, month: u64, day: u64) -> u64 {
    let y = if month <= 2 { year - 1 } else { year } as i64;
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64; // [0, 399]
    let mp = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    (era * 146_097 + doe as i64 - 719_468) as u64
}

fn parse_compact_date(name: &str) -> Option<u64> {
    let digits: String = name.chars().filter(|ch| ch.is_ascii_digit()).collect();
    if digits.len() != 8 {
        return None;
    }
    let year: u64 = digits[..4].parse().ok()?;
    let month: u64 = digits[4..6].parse().ok()?;
    let day: u64 = digits[6..8].parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    Some(days_from_civil(year, month, day))
}

/// `(YYYYMMDD, HH:MM:SS.mmm)` for a UTC instant.
fn utc_parts(now: SystemTime) -> (u64, u64, u64, u64, u64, u64, u64) {
    let millis = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let days = millis / 86_400_000;
    let within = millis % 86_400_000;
    let (year, month, day) = civil_from_days(days);
    (
        year,
        month,
        day,
        within / 3_600_000,
        (within / 60_000) % 60,
        (within / 1000) % 60,
        within % 1000,
    )
}

/// `2026-11-27T13:45:01.234Z` for the log line's `ts`.
pub fn utc_timestamp(now: SystemTime) -> String {
    let (year, month, day, hour, minute, second, millis) = utc_parts(now);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
}

fn file_name(year: u64, month: u64, day: u64) -> String {
    format!("usage-{year:04}{month:02}{day:02}.jsonl")
}

/// Delete every `usage-YYYYMMDD.jsonl` older than `retention_days` before `today` (days since
/// epoch). Best effort: a file that cannot be deleted stays.
fn prune_old_files(directory: &Path, today: u64, retention_days: u32) {
    // Retention of N days keeps today and the N-1 days before it: delete anything strictly
    // older than the first day of the window.
    let cutoff = (today + 1).saturating_sub(u64::from(retention_days));
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(date) = name
            .strip_prefix("usage-")
            .and_then(|rest| rest.strip_suffix(".jsonl"))
            .and_then(parse_compact_date)
        else {
            continue;
        };
        if date < cutoff {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

// ------------------------------------------------------------------------------------------------
// The writer: one background thread behind a bounded, non-blocking channel.
// ------------------------------------------------------------------------------------------------

enum Message {
    Line {
        days_since_epoch: u64,
        year: u64,
        month: u64,
        day: u64,
        line: String,
    },
    Flush(SyncSender<()>),
}

struct Writer {
    directory: PathBuf,
    retention_days: u32,
    current_day: Option<(u64, std::fs::File)>,
}

impl Writer {
    fn write_line(&mut self, days_since_epoch: u64, year: u64, month: u64, day: u64, line: &str) {
        let needs_new_file = self
            .current_day
            .as_ref()
            .is_none_or(|(open_day, _)| *open_day != days_since_epoch);
        if needs_new_file {
            // Rotation: the first write of a new day opens the new file and prunes the old ones.
            prune_old_files(&self.directory, days_since_epoch, self.retention_days);
            match std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(self.directory.join(file_name(year, month, day)))
            {
                Ok(file) => self.current_day = Some((days_since_epoch, file)),
                Err(error) => {
                    eprintln!(
                        "warning: usage log cannot open {}: {error}",
                        self.directory.join(file_name(year, month, day)).display()
                    );
                    self.current_day = None;
                    return;
                }
            }
        }
        if let Some((_, file)) = &mut self.current_day
            && let Err(error) = writeln!(file, "{line}").and_then(|()| file.flush())
        {
            eprintln!("warning: usage log write failed: {error}");
            self.current_day = None;
        }
    }
}

/// The request-side handle: clone-free line submission that never blocks.
pub struct UsageLog {
    sender: SyncSender<Message>,
    dropped: Arc<AtomicU64>,
}

impl UsageLog {
    /// Start the writer on `<directory>` (created if missing), pruning files older than
    /// `retention_days` once at startup.
    pub fn spawn(directory: PathBuf, retention_days: u32) -> std::io::Result<Self> {
        std::fs::create_dir_all(&directory)?;
        let now = SystemTime::now();
        let days = now
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            / 86_400;
        prune_old_files(&directory, days, retention_days);
        let (sender, receiver) = sync_channel::<Message>(CHANNEL_CAPACITY);
        std::thread::Builder::new()
            .name("usage-log".into())
            .spawn(move || {
                let mut writer = Writer {
                    directory,
                    retention_days,
                    current_day: None,
                };
                while let Ok(message) = receiver.recv() {
                    match message {
                        Message::Line {
                            days_since_epoch,
                            year,
                            month,
                            day,
                            line,
                        } => writer.write_line(days_since_epoch, year, month, day, &line),
                        Message::Flush(ack) => {
                            let _ = ack.try_send(());
                        }
                    }
                }
            })?;
        Ok(Self {
            sender,
            dropped: Arc::new(AtomicU64::new(0)),
        })
    }

    /// Hand one record to the writer. Never blocks: a full channel drops the line and counts it.
    pub fn log(&self, record: &UsageRecord, now: SystemTime) {
        let Ok(mut line) = serde_json::to_string(record) else {
            return;
        };
        line.shrink_to_fit();
        let (year, month, day, ..) = utc_parts(now);
        let days_since_epoch = now
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            / 86_400;
        match self.sender.try_send(Message::Line {
            days_since_epoch,
            year,
            month,
            day,
            line,
        }) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
                let dropped = self.dropped.fetch_add(1, Ordering::Relaxed) + 1;
                // Rare enough that one stderr line per order of magnitude cannot flood it, loud
                // enough that an operator sees the log is dropping instead of trusting it.
                if dropped.is_power_of_two() {
                    eprintln!("warning: usage log dropped {dropped} lines (disk too slow)");
                }
            }
        }
    }

    /// Lines dropped because the writer could not keep up.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Block until every line submitted so far is written. Tests use it; a request never waits.
    pub fn flush(&self) {
        let (ack, wait) = sync_channel::<()>(1);
        if self.sender.send(Message::Flush(ack)).is_ok() {
            let _ = wait.recv_timeout(std::time::Duration::from_secs(5));
        }
    }
}

/// Build the usage logger for one runtime, or `None` when the log is off. Writing under
/// `<state>/usage/` means an ephemeral state directory keeps the log ephemeral, too.
pub fn build_usage_log(
    enabled: bool,
    retention_days: u32,
    state_dir: &Path,
) -> Option<Arc<UsageLog>> {
    if !enabled {
        return None;
    }
    match UsageLog::spawn(state_dir.join("usage"), retention_days) {
        Ok(log) => Some(Arc::new(log)),
        Err(error) => {
            // The log is observability, never a reason to keep the server down.
            eprintln!("warning: usage log disabled, cannot open state directory: {error}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record() -> UsageRecord {
        UsageRecord {
            ts: utc_timestamp(SystemTime::now()),
            tool: "read".into(),
            outcome: "ok".into(),
            retry: None,
            ms: 3,
            resp_bytes: 42,
            doc: Some("d3".into()),
            unit: Some("sentence".into()),
            batch_items: None,
        }
    }

    fn read_all(directory: &Path) -> String {
        let mut output = String::new();
        for entry in std::fs::read_dir(directory).unwrap().flatten() {
            output.push_str(&std::fs::read_to_string(entry.path()).unwrap());
        }
        output
    }

    #[test]
    fn utc_calendar_round_trips_and_formats() {
        // 2026-11-27 is day 20788; 1970-01-01 is day 0.
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        let days = days_from_civil(2026, 11, 27);
        assert_eq!(civil_from_days(days), (2026, 11, 27));
        assert_eq!(civil_from_days(days + 30), (2026, 12, 27));
        let instant = SystemTime::UNIX_EPOCH + std::time::Duration::from_millis(1_764_279_901_234);
        assert!(utc_timestamp(instant).ends_with('Z'));
        assert_eq!(utc_timestamp(instant).len(), 24);
    }

    #[test]
    fn doc_key_never_carries_the_checksum() {
        assert_eq!(doc_key_of("d3~9f2c"), Some("d3".to_owned()));
        assert_eq!(doc_key_of("d3~9f2c.s12.p4.2"), Some("d3".to_owned()));
        assert_eq!(doc_key_of("b:d3~9f2c.s12.S.5"), Some("d3".to_owned()));
        assert_eq!(doc_key_of("r:d123~0000.s0.P.1"), Some("d123".to_owned()));
        assert_eq!(doc_key_of("next"), None);
        assert_eq!(doc_key_of("d~9f2c"), None);
    }

    #[test]
    fn a_line_has_exactly_the_allowed_keys() {
        let keys: std::collections::BTreeSet<String> = serde_json::to_value(record())
            .unwrap()
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        for key in &keys {
            assert!(
                USAGE_LINE_KEYS.contains(&key.as_str()),
                "unexpected key {key}"
            );
        }
    }

    #[test]
    fn lines_land_in_todays_file_without_blocking() {
        let directory = tempfile::tempdir().unwrap();
        let log = UsageLog::spawn(directory.path().to_path_buf(), 30).unwrap();
        for _ in 0..10 {
            log.log(&record(), SystemTime::now());
        }
        log.flush();
        let content = read_all(directory.path());
        assert_eq!(content.lines().count(), 10);
        assert_eq!(log.dropped(), 0);
        assert!(
            content
                .lines()
                .all(|line| line.contains("\"tool\":\"read\""))
        );
    }

    #[test]
    fn rotation_and_retention_with_fake_dates() {
        let directory = tempfile::tempdir().unwrap();
        let usage = directory.path().join("usage");
        std::fs::create_dir_all(&usage).unwrap();
        // Three files: today, 29 days ago (kept), 31 days ago (pruned at a 30-day window).
        let today = days_from_civil(2026, 11, 27);
        let name = |days: u64| {
            let (year, month, day) = civil_from_days(days);
            file_name(year, month, day)
        };
        for days in [today, today - 29, today - 31] {
            std::fs::write(usage.join(name(days)), b"{}\n").unwrap();
        }
        prune_old_files(&usage, today, 30);
        assert!(usage.join(name(today)).exists());
        assert!(usage.join(name(today - 29)).exists());
        assert!(!usage.join(name(today - 31)).exists());

        // A writer writing into a new day prunes as the file rotates: same rule, first write of
        // the day.
        let mut writer = Writer {
            directory: usage.clone(),
            retention_days: 30,
            current_day: None,
        };
        let (year, month, day) = civil_from_days(today);
        writer.write_line(today, year, month, day, "{}");
        writer.write_line(today, year, month, day, "{}");
        let content = std::fs::read_to_string(usage.join(name(today))).unwrap();
        assert_eq!(content.lines().count(), 3);
    }

    #[test]
    fn a_slower_retention_window_keeps_less() {
        let directory = tempfile::tempdir().unwrap();
        let usage = directory.path().join("usage");
        std::fs::create_dir_all(&usage).unwrap();
        let today = days_from_civil(2026, 11, 27);
        let (year, month, day) = civil_from_days(today - 2);
        std::fs::write(usage.join(file_name(year, month, day)), b"{}\n").unwrap();
        prune_old_files(&usage, today, 2);
        assert!(!usage.join(file_name(year, month, day)).exists());
    }

    #[test]
    fn off_writes_nothing() {
        let directory = tempfile::tempdir().unwrap();
        assert!(build_usage_log(false, 30, directory.path()).is_none());
        assert!(!directory.path().join("usage").exists());
    }

    #[test]
    fn on_creates_the_usage_directory() {
        let directory = tempfile::tempdir().unwrap();
        let log = build_usage_log(true, 30, directory.path()).unwrap();
        assert!(directory.path().join("usage").is_dir());
        log.log(&record(), SystemTime::now());
        log.flush();
        assert!(!read_all(&directory.path().join("usage")).is_empty());
    }
}
