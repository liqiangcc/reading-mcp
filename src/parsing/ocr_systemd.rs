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
        Self::command_with_collection(python, true)
    }

    pub(super) fn command_in_root(python: &Path, root: &Path) -> io::Result<(Command, Self)> {
        // An operator-selected, already verified immutable runtime directory,
        // never a document path or an implicit host-root fallback.
        if !root.is_absolute()
            || root == Path::new("/")
            || !root.is_dir()
            || root.canonicalize()? != root
            || !python.is_absolute()
        {
            return Err(io::Error::other("invalid private OCR runtime path"));
        }
        Self::command_with_root(python, Some(root), true)
    }

    fn command_with_collection(python: &Path, collect_failed: bool) -> io::Result<(Command, Self)> {
        Self::command_with_root(python, None, collect_failed)
    }

    fn command_with_root(
        python: &Path,
        root: Option<&Path>,
        collect_failed: bool,
    ) -> io::Result<(Command, Self)> {
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
                "--property=MemoryMax=768M",
                "--property=MemorySwapMax=0",
                "--property=TasksMax=64",
                "--property=PrivateNetwork=yes",
                "--property=TemporaryFileSystem=/tmp:rw,size=512M,mode=0700,uid=65534,gid=65534",
                "--property=ProtectHome=tmpfs",
                "--property=ProtectSystem=strict",
                "--property=PrivateDevices=yes",
                "--property=ProtectControlGroups=yes",
                "--property=ProtectKernelTunables=yes",
                "--property=RestrictNamespaces=yes",
                "--property=ReadOnlyPaths=-/dev/shm",
                "--property=CapabilityBoundingSet=CAP_SYS_PTRACE CAP_SETUID CAP_SETGID",
                "--property=AmbientCapabilities=",
                "--property=RuntimeMaxSec=60",
                "--property=TimeoutStopSec=1",
                "--property=KillMode=control-group",
                "--property=LimitNOFILE=256",
                "--property=LimitFSIZE=536870912",
                "--property=LimitCORE=0",
                "--property=NoNewPrivileges=yes",
                "--property=UMask=0077",
            ])
            .arg(format!("--unit={}", unit.name));
        if collect_failed {
            command.arg("--collect");
        }
        if let Some(root) = root {
            let mut property = std::ffi::OsString::from("--property=RootDirectory=");
            property.push(root);
            command.arg(property).arg("--property=MountAPIVFS=yes");
        }
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

    #[cfg(all(test, target_os = "linux"))]
    async fn fault_result_and_reset(&self) -> String {
        // Only tests retain failed units to inspect the manager's actual reason.
        // Production always collects them automatically.
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            Command::new("/usr/bin/systemctl")
                .args(["show", &self.name, "--property=Result", "--value"])
                .kill_on_drop(true)
                .output(),
        )
        .await;
        let reset = tokio::time::timeout(
            Duration::from_secs(2),
            Command::new("/usr/bin/systemctl")
                .args(["reset-failed", &self.name])
                .kill_on_drop(true)
                .output(),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(reset.status.success());
        let result = result.unwrap().unwrap();
        assert!(result.status.success());
        String::from_utf8(result.stdout).unwrap().trim().into()
    }

    /// Stop only our unique unit, including descendants outside the client PGID.
    /// The caller retains admission and the client until this completes.
    pub(super) fn close_owner(&mut self) {
        self.writer = None;
    }

    pub(super) async fn stop(&mut self) -> io::Result<()> {
        // EOF independently stops the service even if this future is cancelled,
        // the runtime is destroyed, or systemctl itself cannot be started.
        self.close_owner();
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

    #[test]
    fn private_root_rejects_host_root_relative_missing_and_symlink_paths() {
        let python = Path::new("/opt/ocr-python/bin/python");
        for root in [
            Path::new("/"),
            Path::new("relative"),
            Path::new("/missing-ocr-test-root"),
        ] {
            assert!(SystemdOcrUnit::command_in_root(python, root).is_err());
        }
        let directory = tempfile::tempdir().unwrap();
        let link = directory.path().join("alias");
        std::os::unix::fs::symlink(directory.path(), &link).unwrap();
        assert!(SystemdOcrUnit::command_in_root(python, &link).is_err());
        assert!(SystemdOcrUnit::command_in_root(Path::new("python"), directory.path()).is_err());
    }

    #[tokio::test]
    #[ignore = "requires hosted root and systemd cgroup v2"]
    async fn runtime_pdf_worker_cannot_restore_root_or_access_supervisor_credentials() {
        let (mut command, unit) = SystemdOcrUnit::command(Path::new("/usr/bin/python3")).unwrap();
        let result = tokio::time::timeout(
            Duration::from_secs(10),
            command
                .args([
                    "-I",
                    "-c",
                    r#"
import json, os, stat, tempfile
from pathlib import Path
assert os.getresuid() == (65534, 65534, 65534)
assert os.getresgid() == (65534, 65534, 65534)
assert os.getgroups() == []
status = dict(line.split(':', 1) for line in Path('/proc/self/status').read_text().splitlines())
caps = {key: int(status[key].strip(), 16) for key in ('CapEff', 'CapPrm', 'CapInh', 'CapAmb')}
assert all(value == 0 for value in caps.values()), caps
assert status['NoNewPrivs'].strip() == '1'
try:
    os.setuid(0)
except PermissionError:
    pass
else:
    raise AssertionError('worker restored root credentials')
for path in (f'/proc/{os.getppid()}/environ', f'/proc/{os.getppid()}/fd/0', '/proc/1/ns/net'):
    try:
        descriptor = os.open(path, os.O_RDONLY | os.O_NONBLOCK)
    except PermissionError:
        pass
    else:
        os.close(descriptor)
        raise AssertionError('worker accessed a privileged process: ' + path)
tmp = os.stat('/tmp')
assert tmp.st_uid == 65534 and tmp.st_gid == 65534
assert stat.S_IMODE(tmp.st_mode) == 0o700
with tempfile.TemporaryFile(dir='/tmp') as stream:
    stream.write(b'bounded unprivileged scratch')
print(json.dumps({'uid': os.getuid(), 'gid': os.getgid(), 'capabilities': caps,
                  'no_new_privileges': True, 'privileged_proc_denied': True}), flush=True)
"#,
                ])
                .stdin(Stdio::null())
                .kill_on_drop(true)
                .output(),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let value: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
        assert_eq!(value["uid"], 65534);
        assert_eq!(value["privileged_proc_denied"], true);
        drop(unit);
    }

    #[tokio::test]
    #[ignore = "requires hosted root and systemd cgroup v2"]
    async fn runtime_unit_enforces_memory_and_aggregate_private_temp_limits() {
        // The existing public mechanism fault payload is reused only in this
        // test binary, now launched through the actual Rust production builder.
        for case in ["bounds", "memory"] {
            let (mut command, unit) = SystemdOcrUnit::command_with_collection(
                Path::new("/usr/bin/python3"),
                case == "bounds",
            )
            .unwrap();
            let result = tokio::time::timeout(
                Duration::from_secs(20),
                command
                    .args([
                        "-I",
                        "-c",
                        include_str!("../../scripts/ocr/sandbox_probe.py"),
                        "--child",
                        case,
                    ])
                    .stdin(Stdio::null())
                    .kill_on_drop(true)
                    .output(),
            )
            .await
            .unwrap()
            .unwrap();
            let reason = if case == "memory" {
                Some(unit.fault_result_and_reset().await)
            } else {
                None
            };
            let value: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
            if case == "bounds" {
                assert!(
                    result.status.success(),
                    "{}",
                    String::from_utf8_lossy(&result.stderr)
                );
                assert_eq!(value["memory_max"], 768 * 1024 * 1024);
                assert_eq!(value["tmp_capacity"], 512 * 1024 * 1024);
                assert_eq!(value["tmp_written"], 512 * 1024 * 1024);
                assert_eq!(value["pids_max"], 64);
            } else {
                // Require the manager's actual OOM result, not a generic nonzero
                // exit or an assumption that systemd forwards child exit codes.
                assert!(!result.status.success());
                assert_eq!(reason.as_deref(), Some("oom-kill"));
                assert_eq!(value["attempted_bytes"], 850 * 1024 * 1024);
            }
            drop(unit);
        }
    }

    #[tokio::test]
    #[ignore = "requires hosted root and systemd cgroup v2"]
    async fn runtime_pid_cap_rejects_forks_and_reaps_escaped_process_groups() {
        let (mut command, unit) =
            SystemdOcrUnit::command_with_collection(Path::new("/usr/bin/python3"), false).unwrap();
        let result = tokio::time::timeout(
            Duration::from_secs(10),
            command
                .args([
                    "-I",
                    "-c",
                    r#"
import errno, json, os, signal, time
children = []
for unused in range(100):
    try:
        pid = os.fork()
    except OSError as error:
        assert error.errno == errno.EAGAIN
        print(json.dumps({'rejected': True, 'children': children}), flush=True)
        break
    if pid == 0:
        os.setsid()
        signal.signal(signal.SIGTERM, signal.SIG_IGN)
        while True: time.sleep(1)
    children.append(pid)
else:
    raise AssertionError('cgroup PID cap did not reject forks')
"#,
                ])
                .stdin(Stdio::null())
                .kill_on_drop(true)
                .output(),
        )
        .await
        .unwrap()
        .unwrap();
        let reason = unit.fault_result_and_reset().await;
        // Leaking resistant descendants must fail the unit even if the direct
        // worker exits zero. Still require actual EAGAIN and complete reaping.
        assert!(!result.status.success());
        assert_eq!(reason, "timeout");
        let report: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
        assert_eq!(report["rejected"], true);
        let children = report["children"].as_array().unwrap();
        assert!(!children.is_empty() && children.len() < 64);
        for pid in children {
            assert!(
                !Path::new(&format!("/proc/{}", pid.as_u64().unwrap())).exists(),
                "escaped descendant not reaped"
            );
        }
        drop(unit);
    }

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

    #[tokio::test(flavor = "current_thread")]
    #[ignore = "requires hosted root and systemd cgroup v2"]
    async fn systemd_cancellation_reaps_descendant_and_retains_admission() {
        let (mut command, unit) = SystemdOcrUnit::command(Path::new("/usr/bin/python3")).unwrap();
        let child = command.args(["-I", "-c", r#"
import os, signal, time, json, resource
from pathlib import Path
assert resource.getrlimit(resource.RLIMIT_CORE) == (0, 0)
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
        // Do not poll the spawned cleanup future. The synchronous owner close
        // must still make the independent service reap the whole descendant tree.
        std::thread::sleep(Duration::from_millis(1400));
        for pid in &pids {
            assert!(
                !Path::new(&format!("/proc/{pid}")).exists(),
                "cleanup depended on polling Tokio"
            );
        }
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
