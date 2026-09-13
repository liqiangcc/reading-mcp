//! Cancellation ownership for the local layout/OCR process group.
//! The legacy PGID path is not a sandbox. An explicitly attached transient unit
//! owns the additional cgroup/network/temp boundary.

use super::ocr_systemd::SystemdOcrUnit;
use std::{io, process::ExitStatus, time::Duration};
use tokio::{process::Child, sync::OwnedSemaphorePermit};

pub(super) struct WorkerProcess {
    child: Option<Child>,
    permit: Option<OwnedSemaphorePermit>,
    group: Option<u32>,
    unit: Option<SystemdOcrUnit>,
    termination_error: Option<crate::application::ports::ApplicationError>,
}

impl WorkerProcess {
    /// The child must have been spawned with process_group(0) on Unix.
    pub(super) fn new(child: Child, permit: OwnedSemaphorePermit) -> Self {
        let group = child.id();
        Self {
            child: Some(child),
            permit: Some(permit),
            group,
            unit: None,
            termination_error: None,
        }
    }

    pub(super) fn with_systemd_unit(mut self, unit: Option<SystemdOcrUnit>) -> Self {
        self.unit = unit;
        self
    }

    pub(super) fn child_mut(&mut self) -> &mut Child {
        self.child
            .as_mut()
            .expect("worker child retained until cleanup")
    }

    pub(super) fn termination_error(&self) -> Option<crate::application::ports::ApplicationError> {
        self.termination_error.clone()
    }

    pub(super) async fn wait(&mut self) -> io::Result<ExitStatus> {
        // Linux WNOWAIT observes exit without releasing the PID. This pins the
        // private group identity until after signaling, avoiding PID reuse.
        #[cfg(target_os = "linux")]
        if let Some(group) = self.group {
            loop {
                // SAFETY: waitid initializes this POD output and only observes
                // our owned child; WNOWAIT leaves reaping to Tokio below.
                let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
                let result = unsafe {
                    libc::waitid(
                        libc::P_PID,
                        group,
                        &mut info,
                        libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                    )
                };
                if result != 0 {
                    let error = io::Error::last_os_error();
                    if error.kind() == io::ErrorKind::Interrupted {
                        continue;
                    }
                    return Err(error);
                }
                if unsafe { info.si_pid() } != 0 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
        #[cfg(target_os = "linux")]
        if let Some(group) = self.group.take() {
            signal_group(group, libc::SIGKILL);
        }
        let status = self.child_mut().wait().await?;
        if let Some(unit) = &mut self.unit {
            self.termination_error = unit.termination_error();
        }
        self.group = None;
        // --wait returns after the unit has terminated; --collect removes the
        // completed unit and its private mounts, including on worker failure.
        if status.success() {
            self.unit = None;
        }
        Ok(status)
    }
}

fn signal_group(group: u32, signal: i32) {
    #[cfg(unix)]
    if let Ok(group) = i32::try_from(group)
        && group > 0
    {
        // SAFETY: a strictly positive child PID is the private process-group ID
        // assigned at spawn. The negative ID targets only that worker group.
        unsafe {
            libc::kill(-group, signal);
        }
    }
    #[cfg(not(unix))]
    let _ = (group, signal);
}

struct KillGroupOnDrop(u32);

impl Drop for KillGroupOnDrop {
    fn drop(&mut self) {
        signal_group(self.0, libc::SIGKILL);
    }
}

impl Drop for WorkerProcess {
    fn drop(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        let group = self.group.take();
        let permit = self.permit.take();
        let unit = self.unit.take();
        if let Some(mut unit) = unit {
            // Initiate independent reaping now, not when Tokio next polls the
            // cleanup future. This also closes the pre-start cancellation race.
            unit.close_owner();
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn(async move {
                    // Stop the service before its systemd-run client. Killing
                    // only the client process group would orphan the OCR tree.
                    let stopped = unit.stop().await.is_ok();
                    if let Some(group) = group {
                        signal_group(group, libc::SIGKILL);
                    }
                    let _ = child.start_kill();
                    let _ = tokio::time::timeout(Duration::from_millis(400), child.wait()).await;
                    if !stopped && let Some(permit) = permit {
                        // Fail closed: unconfirmed cleanup cannot admit another
                        // worker. RuntimeMaxSec remains a separate safety bound.
                        permit.forget();
                    }
                });
            } else {
                // Dropping the unit closes its owner pipe: the service main
                // independently exits and systemd kills/reaps its descendants.
                // Refuse to claim the slot is safe for reuse without observation.
                if let Some(permit) = permit {
                    permit.forget();
                }
                if let Some(group) = group {
                    signal_group(group, libc::SIGKILL);
                }
                let _ = child.start_kill();
            }
            return;
        }
        let Some(group) = group else {
            return;
        };
        signal_group(group, libc::SIGTERM);
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            // Created before spawning: shutdown/cancellation of the cleanup
            // future must still terminate the group, not only its leader.
            let kill_group = KillGroupOnDrop(group);
            runtime.spawn(async move {
                // Keep the leader unreaped during grace, and retain admission
                // until cleanup completes so cancellation cannot overlap jobs.
                let _permit = permit;
                tokio::time::sleep(Duration::from_secs(1)).await;
                drop(kill_group);
                let _ = child.start_kill();
                let _ = tokio::time::timeout(Duration::from_secs(1), child.wait()).await;
            });
        } else {
            signal_group(group, libc::SIGKILL);
            let _ = child.start_kill();
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::{process::Stdio, sync::Arc};
    use tokio::{
        io::{AsyncBufReadExt, BufReader},
        process::Command,
        sync::Semaphore,
    };

    #[tokio::test]
    async fn cancellation_terminates_term_resistant_descendant_and_reaps_leader() {
        let permit = Arc::new(Semaphore::new(1));
        let child = Command::new("/usr/bin/python3")
            .args([
                "-I",
                "-c",
                r#"
import os, signal, time
parent = os.getpid()
if os.fork() == 0:
    signal.signal(signal.SIGTERM, signal.SIG_IGN)
    print(parent, os.getpid(), flush=True)
    while True: time.sleep(1)
while True: time.sleep(1)
"#,
            ])
            .process_group(0)
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut worker = WorkerProcess::new(child, permit.clone().acquire_owned().await.unwrap());
        let mut output = BufReader::new(worker.child_mut().stdout.take().unwrap());
        let mut ready = String::new();
        tokio::time::timeout(Duration::from_secs(5), output.read_line(&mut ready))
            .await
            .unwrap()
            .unwrap();
        let ids: Vec<u32> = ready
            .split_whitespace()
            .map(|id| id.parse().unwrap())
            .collect();
        assert_eq!(ids.len(), 2);
        let started = tokio::time::Instant::now();
        drop(worker);
        assert!(
            permit.clone().try_acquire_owned().is_err(),
            "cleanup must retain admission"
        );
        let _next = tokio::time::timeout(Duration::from_secs(2), permit.acquire())
            .await
            .unwrap()
            .unwrap();
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(
            !std::path::Path::new(&format!("/proc/{}", ids[0])).exists(),
            "leader must be reaped"
        );
        // Orphan reaping belongs to the host's init/subreaper. A zombie is
        // terminated, but this test deliberately does not claim it is reaped.
        tokio::time::timeout_at(started + Duration::from_secs(2), async {
            loop {
                let state = tokio::fs::read_to_string(format!("/proc/{}/stat", ids[1])).await;
                match state {
                    Err(error) if error.kind() == io::ErrorKind::NotFound => break,
                    Ok(state)
                        if state.rsplit_once(") ").unwrap().1.split_whitespace().next()
                            == Some("Z") =>
                    {
                        break;
                    }
                    Err(error) => panic!("cannot verify descendant: {error}"),
                    _ => tokio::time::sleep(Duration::from_millis(10)).await,
                }
            }
        })
        .await
        .expect("descendant must stop within cleanup deadline");
    }

    #[tokio::test]
    async fn completed_worker_releases_admission_without_cancellation_grace() {
        let permit = Arc::new(Semaphore::new(1));
        let child = Command::new("/usr/bin/python3")
            .args(["-I", "-c", "pass"])
            .process_group(0)
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut worker = WorkerProcess::new(child, permit.clone().acquire_owned().await.unwrap());
        assert!(worker.wait().await.unwrap().success());
        drop(worker);
        assert!(permit.try_acquire_owned().is_ok());
    }
}
