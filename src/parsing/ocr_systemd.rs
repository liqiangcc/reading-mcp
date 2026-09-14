//! OCR-only transient service boundary. Native layout does not use this launcher.
use std::{io, path::Path, process::Stdio, time::Duration};
use tokio::process::Command;

pub(super) struct SystemdOcrUnit {
    name: String,
    // Dropping these also works when the Tokio cleanup task cannot run.
    _reader: std::fs::File,
    writer: Option<std::fs::File>,
    result: ResultPipe,
}

struct ResultPipe {
    reader: std::fs::File,
    _writer: std::fs::File,
    token: String,
}

impl ResultPipe {
    #[cfg(target_os = "linux")]
    fn new(token: &str) -> io::Result<(Self, Vec<String>)> {
        use std::os::{fd::AsRawFd, unix::fs::MetadataExt};
        let (reader, writer, _) = owner_pipe("")?;
        let metadata = writer.metadata()?;
        let arguments = vec![
            format!("/proc/{}/fd/{}", std::process::id(), writer.as_raw_fd()),
            metadata.dev().to_string(),
            metadata.ino().to_string(),
            token.into(),
        ];
        Ok((
            Self {
                reader,
                _writer: writer,
                token: token.into(),
            },
            arguments,
        ))
    }

    #[cfg(not(target_os = "linux"))]
    fn new(_: &str) -> io::Result<(Self, Vec<String>)> {
        Err(io::Error::other("OCR result pipe requires Linux"))
    }

    fn read_error(&mut self) -> Option<crate::application::ports::ApplicationError> {
        use std::io::Read;
        let mut buffer = [0_u8; 512];
        let count = self.reader.read(&mut buffer).ok()?;
        classify_manager_result(&buffer[..count], &self.token)
    }
}

fn classify_manager_result(
    bytes: &[u8],
    token: &str,
) -> Option<crate::application::ports::ApplicationError> {
    use crate::application::ports::ApplicationError;
    let text = std::str::from_utf8(bytes).ok()?;
    let fields: Vec<_> = text.split('\n').collect();
    if fields.len() != 5
        || fields[0] != token
        || !fields[4].is_empty()
        || fields[..4]
            .iter()
            .any(|field| field.len() > 64 || !field.is_ascii())
    {
        return None;
    }
    match fields[1] {
        "oom-kill" => Some(ApplicationError::OcrResourceLimit),
        "timeout" => Some(ApplicationError::OcrTimeout),
        // An ordinary worker exit still uses its validated typed protocol.
        // No mapping from numerical exit codes, signals or arbitrary strings.
        _ => None,
    }
}

fn unit_argument(value: &str) -> String {
    format!(
        "\"{}\"",
        value
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n")
            .replace('\r', "\\r")
            .replace('%', "%%")
            .replace('$', "$$")
    )
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
        Self::validate_runtime_root(python, root)?;
        Self::command_with_root(python, Some(root), None, true)
    }

    fn validate_runtime_root(python: &Path, root: &Path) -> io::Result<()> {
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
        Ok(())
    }

    pub(super) fn command_in_package(
        python: &Path,
        root: &Path,
        manifest: &Path,
    ) -> io::Result<(Command, Self)> {
        // BindReadOnlyPaths uses systemd's path-list syntax, not shell quoting.
        // Refuse ambiguous/specifier-bearing operator paths instead of guessing.
        let path = manifest
            .to_str()
            .ok_or_else(|| io::Error::other("invalid OCR manifest path"))?;
        if !manifest.is_absolute()
            || manifest.canonicalize()? != manifest
            || !manifest.is_file()
            || path
                .chars()
                .any(|c| c.is_whitespace() || matches!(c, ':' | '%' | '\\' | '\'' | '"'))
        {
            return Err(io::Error::other("invalid OCR manifest path"));
        }
        Self::validate_runtime_root(python, root)?;
        Self::command_with_root(python, Some(root), Some(path), true)
    }

    fn command_with_collection(python: &Path, collect_failed: bool) -> io::Result<(Command, Self)> {
        Self::command_with_root(python, None, None, collect_failed)
    }

    /// Only `/tmp` (and `/run` when a package manifest is bind-mounted) get a
    /// writable tmpfs; the runtime rootfs itself stays under ProtectSystem.
    fn scratch_property(with_run: bool) -> &'static str {
        if with_run {
            "--property=TemporaryFileSystem=/tmp:rw,size=512M,mode=0700,uid=65534,gid=65534 /run:rw,size=1M,mode=0755"
        } else {
            "--property=TemporaryFileSystem=/tmp:rw,size=512M,mode=0700,uid=65534,gid=65534"
        }
    }

    fn isolation_properties(with_run: bool) -> Vec<&'static str> {
        vec![
            "--property=MemoryMax=768M",
            "--property=MemorySwapMax=0",
            "--property=TasksMax=64",
            "--property=PrivateNetwork=yes",
            Self::scratch_property(with_run),
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
        ]
    }

    fn command_with_root(
        python: &Path,
        root: Option<&Path>,
        manifest: Option<&str>,
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
        let (result, result_arguments) = ResultPipe::new(token)?;
        let unit = Self {
            name: format!("reading-mcp-ocr-{token}.service"),
            _reader: reader,
            writer: Some(writer),
            result,
        };
        let mut command = Command::new("/usr/bin/systemd-run");
        command
            .args(["--quiet", "--wait", "--pipe"])
            .args(Self::isolation_properties(manifest.is_some()))
            .arg(format!("--unit={}", unit.name));
        let result_python = python
            .to_str()
            .ok_or_else(|| io::Error::other("invalid OCR Python path"))?;
        let mut callback: Vec<String> = crate::infrastructure::OCR_INTERPRETER_ARGS
            .iter()
            .map(|argument| (*argument).into())
            .collect();
        callback.insert(0, result_python.to_string());
        callback.push("-c".into());
        callback.push(include_str!("ocr_service_result.py").into());
        callback.extend(result_arguments);
        command.arg(format!(
            "--property=ExecStopPost={}",
            callback
                .iter()
                .map(|value| unit_argument(value))
                .collect::<Vec<_>>()
                .join(" ")
        ));
        if collect_failed {
            command.arg("--collect");
        }
        if let Some(root) = root {
            let mut property = std::ffi::OsString::from("--property=RootDirectory=");
            property.push(root);
            command.arg(property).arg("--property=MountAPIVFS=yes");
        }
        if let Some(manifest) = manifest {
            command.arg(format!(
                "--property=BindReadOnlyPaths={manifest}:/run/reading-mcp-ocr-package.json"
            ));
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
            .args(crate::infrastructure::OCR_INTERPRETER_ARGS)
            .args(["-c", include_str!("ocr_service_supervisor.py")])
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

    pub(super) fn termination_error(
        &mut self,
    ) -> Option<crate::application::ports::ApplicationError> {
        self.result.read_error()
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
    fn manager_result_requires_exact_control_record_and_never_guesses_from_exit_status() {
        use crate::application::ports::ApplicationError;
        assert_eq!(
            classify_manager_result(b"token\noom-kill\nkilled\n9\n", "token"),
            Some(ApplicationError::OcrResourceLimit)
        );
        assert_eq!(
            classify_manager_result(b"token\ntimeout\nkilled\n15\n", "token"),
            Some(ApplicationError::OcrTimeout)
        );
        for bytes in [
            b"wrong\noom-kill\nkilled\n9\n".as_slice(),
            b"token\nexit-code\nexited\n137\n",
            b"token\nsignal\nkilled\n9\n",
            b"token\noom-kill\nkilled\n9\nextra",
            b"token\noom-kill\n",
            b"token\nunknown\n\n\n",
        ] {
            assert_eq!(classify_manager_result(bytes, "token"), None);
        }
    }

    #[tokio::test]
    #[ignore = "requires hosted root and systemd cgroup v2"]
    async fn runtime_manager_result_survives_collection_and_distinguishes_oom_timeout_and_exit() {
        use crate::application::ports::ApplicationError;
        for (script, timeout, expected) in [
            (
                "data = bytearray(850 * 1024 * 1024)",
                false,
                Some(ApplicationError::OcrResourceLimit),
            ),
            (
                "import time; time.sleep(10)",
                true,
                Some(ApplicationError::OcrTimeout),
            ),
            ("raise SystemExit(137)", false, None),
        ] {
            let (mut command, unit) =
                SystemdOcrUnit::command(Path::new("/usr/bin/python3")).unwrap();
            let name = unit.name.clone();
            if timeout {
                // Only the test shortens RuntimeMaxSec; production remains 60s.
                // Properties must precede the existing command separator.
                let arguments: Vec<_> = command
                    .as_std()
                    .get_args()
                    .map(|arg| arg.to_os_string())
                    .collect();
                let mut replacement = Command::new("/usr/bin/systemd-run");
                for argument in arguments {
                    if argument == "--property=RuntimeMaxSec=60" {
                        replacement.arg("--property=RuntimeMaxSec=1");
                    } else {
                        replacement.arg(argument);
                    }
                }
                command = replacement;
            }
            let child = command
                .args(["-I", "-c", script])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .process_group(0)
                .kill_on_drop(true)
                .spawn()
                .unwrap();
            let permit = Arc::new(Semaphore::new(1)).acquire_owned().await.unwrap();
            let mut process = WorkerProcess::new(child, permit).with_systemd_unit(Some(unit));
            let status = tokio::time::timeout(Duration::from_secs(20), process.wait())
                .await
                .unwrap()
                .unwrap();
            assert!(!status.success());
            assert_eq!(process.termination_error(), expected, "{script}");
            let state = Command::new("/usr/bin/systemctl")
                .args(["show", &name, "--property=LoadState", "--value"])
                .output()
                .await
                .unwrap();
            assert_eq!(
                state.stdout, b"not-found\n",
                "failed units must still auto-collect"
            );
        }
    }

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
assert set(os.environ) <= {'PATH', 'LANG', 'OMP_THREAD_LIMIT', 'LC_CTYPE', 'PYTHONDONTWRITEBYTECODE'}
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

    #[test]
    fn private_runtime_unit_keeps_rootfs_read_only_and_scratch_tmpfs() {
        // The package-manifest variant must expose a writable /run for the
        // bind-mounted package identity while the rootfs stays read-only.
        let properties = SystemdOcrUnit::isolation_properties(true);
        assert!(
            properties.contains(&"--property=ProtectSystem=strict"),
            "private OCR runtime rootfs must stay read-only"
        );
        let scratch: Vec<_> = properties
            .iter()
            .filter(|argument| argument.starts_with("--property=TemporaryFileSystem="))
            .collect();
        assert_eq!(scratch.len(), 1);
        assert!(
            scratch[0].contains("/tmp:rw") && scratch[0].contains("/run:rw"),
            "only /tmp and /run may be writable: {scratch:?}"
        );
        let without_manifest = SystemdOcrUnit::isolation_properties(false);
        let scratch: Vec<_> = without_manifest
            .iter()
            .filter(|argument| argument.starts_with("--property=TemporaryFileSystem="))
            .collect();
        assert_eq!(scratch.len(), 1);
        assert!(scratch[0].contains("/tmp:rw") && !scratch[0].contains("/run"));
        // Every interpreter launched inside the private runtime disables
        // bytecode writes: -I already ignores PYTHONDONTWRITEBYTECODE.
        assert_eq!(crate::infrastructure::OCR_INTERPRETER_ARGS, ["-I", "-B"]);
    }
}
