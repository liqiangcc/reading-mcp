//! Bounded startup-only dependency discovery; never used for PDF ingestion.
use std::process::{Command, Output};
use std::time::Duration;

#[cfg(not(target_os = "linux"))]
pub(super) fn dependency_output(_: &mut Command, _: Duration) -> Result<Output, String> {
    Err("OCR dependency discovery requires Linux".into())
}

#[cfg(target_os = "linux")]
pub(super) fn dependency_output(command: &mut Command, budget: Duration) -> Result<Output, String> {
    use std::io::{ErrorKind, Read};
    use std::os::fd::AsRawFd;
    use std::os::unix::process::CommandExt;
    use std::process::{Child, Stdio};
    use std::time::Instant;

    const LIMIT: usize = 64 * 1024;
    struct OwnedChild(Child, bool);
    impl Drop for OwnedChild {
        fn drop(&mut self) {
            if self.1 {
                return;
            }
            // The child remains unreaped until here, reserving its process-group ID.
            // Kill the owned group, including children retaining our output pipes.
            unsafe { libc::kill(-(self.0.id() as i32), libc::SIGKILL) };
            let _ = self.0.wait();
        }
    }
    fn nonblocking(fd: i32) -> Result<(), String> {
        // SAFETY: fd is a live owned ChildStdout/ChildStderr pipe descriptor.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
            return Err("OCR dependency pipe setup failed".into());
        }
        Ok(())
    }
    fn drain(reader: &mut impl Read, bytes: &mut Vec<u8>) -> Result<bool, String> {
        let mut buffer = [0_u8; 8192];
        match reader.read(&mut buffer) {
            Ok(0) => Ok(true),
            Ok(count) => {
                if bytes.len() + count > LIMIT {
                    return Err("OCR dependency output limit exceeded".into());
                }
                bytes.extend_from_slice(&buffer[..count]);
                Ok(false)
            }
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::Interrupted) => {
                Ok(false)
            }
            Err(_) => Err("OCR dependency output read failed".into()),
        }
    }
    let deadline = Instant::now() + budget;
    let mut owned = OwnedChild(
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .map_err(|_| "OCR dependency process start failed")?,
        false,
    );
    let mut stdout = owned
        .0
        .stdout
        .take()
        .ok_or("OCR dependency stdout missing")?;
    let mut stderr = owned
        .0
        .stderr
        .take()
        .ok_or("OCR dependency stderr missing")?;
    nonblocking(stdout.as_raw_fd())?;
    nonblocking(stderr.as_raw_fd())?;
    let (mut out, mut err) = (Vec::new(), Vec::new());
    loop {
        if Instant::now() >= deadline {
            return Err("OCR dependency discovery timed out".into());
        }
        let out_done = drain(&mut stdout, &mut out)?;
        let err_done = drain(&mut stderr, &mut err)?;
        // Observe exit WITHOUT reaping: otherwise PID/group reuse races cleanup.
        let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
        // SAFETY: valid writable siginfo, own child PID; WNOWAIT preserves ownership.
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                owned.0.id(),
                info.as_mut_ptr(),
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if result < 0 {
            return Err("OCR dependency process observation failed".into());
        }
        // SAFETY: waitid initialized the zeroed siginfo on success.
        let exited = unsafe { info.assume_init().si_pid() != 0 };
        if exited && out_done && err_done {
            // Reap only after terminating any remaining group members. No fallible
            // operation between successful wait and disarming the Drop guard.
            unsafe { libc::kill(-(owned.0.id() as i32), libc::SIGKILL) };
            let status = owned.0.wait().map_err(|_| "OCR dependency reap failed")?;
            owned.1 = true;
            return Ok(Output {
                status,
                stdout: out,
                stderr: err,
            });
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    fn shell(script: &str) -> Command {
        let mut command = Command::new("/bin/sh");
        command.env_clear().arg("-c").arg(script);
        command
    }

    #[test]
    fn discovery_preserves_both_streams_and_nonzero_status() {
        let output = dependency_output(
            &mut shell("printf library; printf missing >&2; exit 7"),
            Duration::from_secs(2),
        )
        .unwrap();
        assert_eq!(output.stdout, b"library");
        assert_eq!(output.stderr, b"missing");
        assert_eq!(output.status.code(), Some(7));
    }

    #[test]
    fn discovery_bounds_each_output_stream() {
        for redirect in ["", " >&2"] {
            let script =
                format!("while :; do printf '12345678901234567890123456789012'{redirect}; done");
            assert_eq!(
                dependency_output(&mut shell(&script), Duration::from_secs(2)).unwrap_err(),
                "OCR dependency output limit exceeded"
            );
        }
    }

    #[test]
    fn discovery_timeout_kills_descendant_holding_output_pipe() {
        let directory = tempfile::tempdir().unwrap();
        let pidfile = directory.path().join("child-pid");
        let mut command = shell("/bin/sleep 30 & echo $! > \"$1\"; exit 0");
        command.arg("test").arg(&pidfile);
        let started = std::time::Instant::now();
        assert_eq!(
            dependency_output(&mut command, Duration::from_millis(200)).unwrap_err(),
            "OCR dependency discovery timed out"
        );
        assert!(started.elapsed() < Duration::from_secs(2));
        let pid: u32 = std::fs::read_to_string(pidfile)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        loop {
            let state = std::fs::read_to_string(format!("/proc/{pid}/stat"));
            // Orphan zombies are reaped by init; they cannot run or hold descriptors.
            if match state {
                Err(_) => true,
                Ok(s) => s.split_once(") ").unwrap().1.starts_with('Z'),
            } {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "dependency descendant still running"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
