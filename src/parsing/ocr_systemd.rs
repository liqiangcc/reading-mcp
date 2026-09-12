//! OCR-only transient service boundary. Native layout does not use this launcher.
use std::{io, path::Path, process::Stdio, time::Duration};
use tokio::process::Command;

pub(super) struct SystemdOcrUnit {
    name: String,
    // Dropping these also works when the Tokio cleanup task cannot run.
    _reader: std::fs::File,
    writer: Option<std::fs::File>,
}

#[cfg(target_os = "linux")]
fn owner_pipe(token: &str) -> io::Result<(std::fs::File, std::fs::File, Vec<String>)> {
    use std::{
        io::Write,
        os::{
            fd::{AsRawFd, FromRawFd},
            unix::fs::MetadataExt,
        },
    };
    let mut descriptors = [-1; 2];
    // SAFETY: pipe2 writes exactly two descriptors to this initialized array.
    if unsafe { libc::pipe2(descriptors.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: each successful pipe2 descriptor is transferred once to an owner.
    let reader = unsafe { std::fs::File::from_raw_fd(descriptors[0]) };
    let mut writer = unsafe { std::fs::File::from_raw_fd(descriptors[1]) };
    writer.write_all(token.as_bytes())?;
    let metadata = reader.metadata()?;
    let arguments = vec![
        format!("/proc/{}/fd/{}", std::process::id(), reader.as_raw_fd()),
        metadata.dev().to_string(),
        metadata.ino().to_string(),
        token.into(),
    ];
    Ok((reader, writer, arguments))
}

#[cfg(not(target_os = "linux"))]
fn owner_pipe(_: &str) -> io::Result<(std::fs::File, std::fs::File, Vec<String>)> {
    Err(io::Error::other("OCR systemd boundary requires Linux"))
}

impl SystemdOcrUnit {
    pub(super) fn validate_host() -> io::Result<()> {
        #[cfg(target_os = "linux")]
        let available = unsafe { libc::geteuid() } == 0
            && Path::new("/run/systemd/system").is_dir()
            && Path::new("/sys/fs/cgroup/cgroup.controllers").is_file();
        #[cfg(not(target_os = "linux"))]
        let available = false;
        if !available {
            return Err(io::Error::other(
                "OCR requires the configured systemd/cgroup-v2 boundary",
            ));
        }
        for path in ["/usr/bin/systemd-run", "/usr/bin/systemctl", "/usr/bin/env"] {
            if !Path::new(path).is_file() {
                return Err(io::Error::other(
                    "OCR systemd launcher dependency is missing",
                ));
            }
        }
        Ok(())
    }

    pub(super) fn command(python: &Path) -> io::Result<(Command, Self)> {
        Self::validate_host()?;
        // Kernel-generated identity, never an input document/operator unit name.
        let token = std::fs::read_to_string("/proc/sys/kernel/random/uuid")?;
        let token = token.trim();
        if token.len() != 36 || !token.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-') {
            return Err(io::Error::other("invalid kernel unit identity"));
        }
        let (reader, writer, owner_arguments) = owner_pipe(token)?;
        let unit = Self {
            name: format!("reading-mcp-ocr-{token}.service"),
            _reader: reader,
            writer: Some(writer),
        };
        let mut command = Command::new("/usr/bin/systemd-run");
        command
            .args([
                "--quiet",
                "--wait",
                "--pipe",
                "--collect",
                "--property=MemoryMax=768M",
                "--property=MemorySwapMax=0",
                "--property=TasksMax=64",
                "--property=PrivateNetwork=yes",
                "--property=TemporaryFileSystem=/tmp:rw,size=512M,mode=0700",
                "--property=RuntimeMaxSec=60",
                "--property=TimeoutStopSec=1",
                "--property=KillMode=control-group",
                "--property=LimitNOFILE=256",
                "--property=LimitFSIZE=536870912",
                "--property=NoNewPrivileges=yes",
                "--property=UMask=0077",
            ])
            .arg(format!("--unit={}", unit.name));
        // The manager's environment is distinct from the client's. Explicitly
        // set the same allowlisted dependency-discovery environment in the unit.
        for (key, value) in crate::infrastructure::OCR_PROCESS_ENV {
            command.arg(format!("--setenv={key}={value}"));
        }
        // Do not inherit a system manager's independently configured variables.
        command.args(["--", "/usr/bin/env", "-i"]);
        for (key, value) in crate::infrastructure::OCR_PROCESS_ENV {
            command.arg(format!("{key}={value}"));
        }
        command
            .arg(python)
            .args(["-I", "-c", include_str!("ocr_service_supervisor.py")])
            .args(owner_arguments)
            .arg(python);
        Ok((command, unit))
    }

    /// Stop only our unique unit, including descendants outside the client PGID.
    /// The caller retains admission and the client until this completes.
    pub(super) async fn stop(&mut self) -> io::Result<()> {
        // EOF independently stops the service even if this future is cancelled,
        // the runtime is destroyed, or systemctl itself cannot be started.
        self.writer = None;
        tokio::time::timeout(Duration::from_millis(1500), async {
            let status = Command::new("/usr/bin/systemctl")
                .args(["stop", &self.name])
                .env_clear()
                .envs(crate::infrastructure::OCR_PROCESS_ENV)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .status()
                .await?;
            if status.success() {
                return Ok(());
            }
            // Closing the pipe may already have terminated/collected the unit.
            // Absence is safe because a late supervisor cannot pass the closed
            // owner-pipe check and start a new worker.
            let state = Command::new("/usr/bin/systemctl")
                .args(["show", &self.name, "--property=LoadState", "--value"])
                .env_clear()
                .envs(crate::infrastructure::OCR_PROCESS_ENV)
                .stdin(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .output()
                .await?;
            if state.stdout == b"not-found\n" {
                Ok(())
            } else {
                Err(io::Error::other("OCR unit cleanup could not be confirmed"))
            }
        })
        .await
        .map_err(|_| io::Error::other("OCR unit cleanup timeout"))?
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use crate::parsing::ocr_worker_process::WorkerProcess;
    use std::sync::Arc;
    use tokio::{
        io::{AsyncBufReadExt, BufReader},
        sync::Semaphore,
    };

    #[tokio::test]
    #[ignore = "requires hosted root and systemd cgroup v2"]
    async fn owner_closed_before_unit_start_never_executes_worker() {
        let (mut command, unit) = SystemdOcrUnit::command(Path::new("/usr/bin/python3")).unwrap();
        drop(unit);
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            command
                .args(["-I", "-c", "print('worker must not start')"])
                .stdin(Stdio::null())
                .kill_on_drop(true)
                .output(),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(!result.status.success());
        assert!(
            result.stdout.is_empty(),
            "cancelled launch must not execute worker"
        );
    }

    #[tokio::test]
    #[ignore = "requires hosted root and systemd cgroup v2"]
    async fn owner_pipe_drop_reaps_tree_without_async_cleanup_or_systemctl() {
        let (mut command, unit) = SystemdOcrUnit::command(Path::new("/usr/bin/python3")).unwrap();
        let mut child = command
            .args([
                "-I",
                "-c",
                r#"
import os, signal, time, json
parent = os.getpid()
pid = os.fork()
if pid == 0:
    signal.signal(signal.SIGTERM, signal.SIG_IGN)
    while True: time.sleep(1)
print(json.dumps([parent, pid]), flush=True)
while True: time.sleep(1)
"#,
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut line = String::new();
        let mut reader = BufReader::new(child.stdout.take().unwrap());
        tokio::time::timeout(Duration::from_secs(5), reader.read_line(&mut line))
            .await
            .unwrap()
            .unwrap();
        let pids: Vec<u32> = serde_json::from_str(&line).unwrap();
        assert_eq!(pids.len(), 2);
        let start = tokio::time::Instant::now();
        // No WorkerProcess Drop, no runtime-spawned future and no systemctl call.
        drop(unit);
        let status = tokio::time::timeout(Duration::from_secs(2), child.wait())
            .await
            .unwrap()
            .unwrap();
        assert!(!status.success());
        for pid in pids {
            assert!(!Path::new(&format!("/proc/{pid}")).exists());
        }
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[tokio::test]
    #[ignore = "requires hosted root and systemd cgroup v2"]
    async fn systemd_cancellation_reaps_descendant_and_retains_admission() {
        let (mut command, unit) = SystemdOcrUnit::command(Path::new("/usr/bin/python3")).unwrap();
        let child = command.args(["-I", "-c", r#"
import os, signal, time, json
from pathlib import Path
group = Path('/sys/fs/cgroup') / Path('/proc/self/cgroup').read_text().split('::', 1)[1].strip().lstrip('/')
assert int((group / 'memory.max').read_text()) == 768 * 1024 * 1024
assert int((group / 'pids.max').read_text()) == 64
assert sorted(p.name for p in Path('/sys/class/net').iterdir()) == ['lo']
assert os.statvfs('/tmp').f_blocks * os.statvfs('/tmp').f_frsize == 512 * 1024 * 1024
assert set(os.environ) <= {'PATH', 'LANG', 'OMP_THREAD_LIMIT', 'LC_CTYPE'}
parent = os.getpid()
pid = os.fork()
if pid == 0:
    signal.signal(signal.SIGTERM, signal.SIG_IGN)
    while True: time.sleep(1)
print(json.dumps([parent, pid]), flush=True)
while True: time.sleep(1)
"#]).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped())
            .process_group(0).kill_on_drop(true).spawn().unwrap();
        let permits = Arc::new(Semaphore::new(1));
        let mut process = WorkerProcess::new(child, permits.clone().acquire_owned().await.unwrap())
            .with_systemd_unit(Some(unit));
        let mut reader = BufReader::new(process.child_mut().stdout.take().unwrap());
        let mut line = String::new();
        tokio::time::timeout(Duration::from_secs(5), reader.read_line(&mut line))
            .await
            .unwrap()
            .unwrap();
        let pids: Vec<u32> = serde_json::from_str(&line).unwrap();
        assert_eq!(pids.len(), 2);
        let start = tokio::time::Instant::now();
        drop(process);
        assert!(permits.try_acquire().is_err());
        let _permit = tokio::time::timeout(Duration::from_secs(2), permits.acquire())
            .await
            .unwrap()
            .unwrap();
        for pid in pids {
            assert!(
                !Path::new(&format!("/proc/{pid}")).exists(),
                "every descendant must be reaped"
            );
        }
        assert!(start.elapsed() < Duration::from_secs(2));
    }
}
