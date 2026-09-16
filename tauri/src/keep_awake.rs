//! Keep Awake Module
//!
//! Prevents the system from going to sleep due to idle, cross-platform:
//! - Windows: SetThreadExecutionState (ES_CONTINUOUS | ES_SYSTEM_REQUIRED)
//! - macOS: IOPMAssertionCreateWithName (PreventUserIdleSystemSleep)
//! - Linux: systemd-logind D-Bus Inhibit("idle")
//!
//! Only `idle` is inhibited — explicit user sleep / lid close still works.
//! The guard is created and dropped on one dedicated thread. Windows execution
//! state belongs to the calling thread, not to the process or the Rust value.

use keepawake::{Builder, KeepAwake};
use std::sync::{mpsc, OnceLock};
use thiserror::Error;
use tokio::sync::oneshot;

#[derive(Error, Debug, Clone)]
pub enum KeepAwakeError {
    #[error("Failed to create keep awake guard: {0}")]
    Create(String),
    #[error("Keep awake worker unavailable: {0}")]
    Worker(String),
}

struct KeepAwakeRequest {
    enabled: bool,
    completion: oneshot::Sender<Result<(), KeepAwakeError>>,
}

#[derive(Clone)]
struct KeepAwakeWorker {
    requests: mpsc::Sender<KeepAwakeRequest>,
}

impl KeepAwakeWorker {
    fn spawn<Guard: 'static>(
        mut create_guard: impl FnMut() -> Result<Guard, KeepAwakeError> + Send + 'static,
    ) -> Result<Self, KeepAwakeError> {
        let (requests, receiver) = mpsc::channel::<KeepAwakeRequest>();
        std::thread::Builder::new()
            .name("keep-awake".to_string())
            .spawn(move || {
                // The guard never leaves this thread, even when callers are
                // startup tasks, IPC commands, or different runtime workers.
                let mut guard = None;
                for request in receiver {
                    let result = if request.enabled {
                        if guard.is_some() {
                            Ok(())
                        } else {
                            create_guard().map(|created| guard = Some(created))
                        }
                    } else {
                        guard = None;
                        Ok(())
                    };
                    let _ = request.completion.send(result);
                }
                // A disconnected controller also drops its guard on this thread.
            })
            .map_err(|error| KeepAwakeError::Worker(error.to_string()))?;
        Ok(Self { requests })
    }

    async fn set_enabled(&self, enabled: bool) -> Result<(), KeepAwakeError> {
        let (completion, result) = oneshot::channel();
        self.requests
            .send(KeepAwakeRequest {
                enabled,
                completion,
            })
            .map_err(|error| KeepAwakeError::Worker(error.to_string()))?;
        result
            .await
            .map_err(|error| KeepAwakeError::Worker(error.to_string()))?
    }
}

fn create_keep_awake_guard() -> Result<KeepAwake, KeepAwakeError> {
    Builder::default()
        .idle(true)
        .reason("AI Toolbox Gateway Router is running")
        .app_name("AI Toolbox Gateway Router")
        .create()
        .map_err(|error| KeepAwakeError::Create(error.to_string()))
}

static KEEP_AWAKE_WORKER: OnceLock<Result<KeepAwakeWorker, KeepAwakeError>> = OnceLock::new();

/// Apply the requested state without blocking the caller's async runtime.
pub async fn set_keep_awake(enabled: bool) -> Result<(), KeepAwakeError> {
    KEEP_AWAKE_WORKER
        .get_or_init(|| KeepAwakeWorker::spawn(create_keep_awake_guard))
        .as_ref()
        .map_err(Clone::clone)?
        .set_enabled(enabled)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::{self, ThreadId};
    use std::time::Duration;

    struct TrackedGuard {
        dropped: mpsc::Sender<ThreadId>,
    }

    impl Drop for TrackedGuard {
        fn drop(&mut self) {
            let _ = self.dropped.send(thread::current().id());
        }
    }

    #[tokio::test]
    async fn different_callers_create_and_release_on_one_worker_thread() {
        let (created_sender, created) = mpsc::channel();
        let (dropped_sender, dropped) = mpsc::channel();
        let worker = KeepAwakeWorker::spawn(move || {
            created_sender.send(thread::current().id()).unwrap();
            Ok(TrackedGuard {
                dropped: dropped_sender.clone(),
            })
        })
        .unwrap();

        let startup_worker = worker.clone();
        let startup_thread = thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap()
                .block_on(startup_worker.set_enabled(true))
                .unwrap();
            thread::current().id()
        })
        .join()
        .unwrap();
        let owner_thread = created.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_ne!(owner_thread, startup_thread);
        assert_ne!(owner_thread, thread::current().id());

        // Re-enabling is idempotent and must not replace a valid assertion.
        worker.set_enabled(true).await.unwrap();
        assert!(created.try_recv().is_err());
        assert!(dropped.try_recv().is_err());

        worker.set_enabled(false).await.unwrap();
        assert_eq!(dropped.try_recv().unwrap(), owner_thread);
    }

    #[tokio::test]
    async fn failed_enable_does_not_poison_later_requests() {
        let mut attempts = 0;
        let worker = KeepAwakeWorker::spawn(move || {
            attempts += 1;
            if attempts == 1 {
                Err(KeepAwakeError::Create("temporary OS failure".to_string()))
            } else {
                Ok(())
            }
        })
        .unwrap();

        assert!(matches!(
            worker.set_enabled(true).await,
            Err(KeepAwakeError::Create(_))
        ));
        worker.set_enabled(true).await.unwrap();
        worker.set_enabled(false).await.unwrap();
    }

    #[tokio::test]
    async fn disconnect_releases_the_guard_on_its_owner_thread() {
        let (created_sender, created) = mpsc::channel();
        let (dropped_sender, dropped) = mpsc::channel();
        let worker = KeepAwakeWorker::spawn(move || {
            created_sender.send(thread::current().id()).unwrap();
            Ok(TrackedGuard {
                dropped: dropped_sender.clone(),
            })
        })
        .unwrap();
        worker.set_enabled(true).await.unwrap();
        let owner_thread = created.try_recv().unwrap();

        drop(worker);
        assert_eq!(
            dropped.recv_timeout(Duration::from_secs(2)).unwrap(),
            owner_thread
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_disable_clears_the_native_execution_state() {
        const ES_CONTINUOUS: u32 = 0x80000000;
        const ES_SYSTEM_REQUIRED: u32 = 0x00000001;

        #[link(name = "kernel32")]
        extern "system" {
            fn SetThreadExecutionState(flags: u32) -> u32;
        }

        struct NativeGuard {
            guard: Option<KeepAwake>,
            released_state: mpsc::Sender<u32>,
        }

        impl Drop for NativeGuard {
            fn drop(&mut self) {
                drop(self.guard.take());
                // Read and clear this dedicated thread's previous flags. This
                // also cleans up the test's request if a regression occurs.
                let previous = unsafe { SetThreadExecutionState(ES_CONTINUOUS) };
                let _ = self.released_state.send(previous);
            }
        }

        let (released_sender, released) = mpsc::channel();
        let worker = KeepAwakeWorker::spawn(move || {
            Ok(NativeGuard {
                guard: Some(create_keep_awake_guard()?),
                released_state: released_sender.clone(),
            })
        })
        .unwrap();
        worker.set_enabled(true).await.unwrap();
        worker.set_enabled(false).await.unwrap();

        let previous_state = released.try_recv().unwrap();
        assert_ne!(previous_state, 0, "native execution-state query succeeded");
        assert_eq!(previous_state & ES_SYSTEM_REQUIRED, 0);
    }
}
