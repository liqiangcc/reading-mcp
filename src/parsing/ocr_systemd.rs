//! OCR-only transient service boundary. Native layout does not use this launcher.
use std::{io, path::Path, process::Stdio, time::Duration};
use tokio::process::Command;

pub(super) struct SystemdOcrUnit {
    name: String,
}

impl SystemdOcrUnit {
    pub(super) fn command(python: &Path) -> io::Result<(Command, Self)> {
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
        // Kernel-generated identity, never an input document/operator unit name.
        let token = std::fs::read_to_string("/proc/sys/kernel/random/uuid")?;
        let token = token.trim();
        if token.len() != 36 || !token.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-') {
            return Err(io::Error::other("invalid kernel unit identity"));
        }
        let unit = Self {
            name: format!("reading-mcp-ocr-{token}.service"),
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
        command.arg(python);
        Ok((command, unit))
    }

    /// Stop only our unique unit, including descendants outside the client PGID.
    /// The caller retains admission and the client until this completes.
    pub(super) async fn stop(&self) -> io::Result<()> {
        let status = tokio::time::timeout(
            Duration::from_millis(1500),
            Command::new("/usr/bin/systemctl")
                .args(["stop", &self.name])
                .env_clear()
                .envs(crate::infrastructure::OCR_PROCESS_ENV)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .status(),
        )
        .await
        .map_err(|_| io::Error::other("OCR unit cleanup timeout"))??;
        if status.success() {
            Ok(())
        } else {
            Err(io::Error::other("OCR unit cleanup could not be confirmed"))
        }
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
