//! Progress reporting and cancellation shared between workers and front ends.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug, Default)]
pub struct ProgressState {
    pub stage: String,
    pub done: usize,
    pub total: usize,
    pub log: Vec<String>,
}

/// Cheap, cloneable handle. Workers call `stage`/`tick`/`log`; front ends poll
/// `snapshot` and may call `cancel`.
#[derive(Clone, Default)]
pub struct Progress {
    state: Arc<Mutex<ProgressState>>,
    cancelled: Arc<AtomicBool>,
    notify: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl Progress {
    pub fn new() -> Self {
        Self::default()
    }

    /// `notify` is invoked after every update (e.g. to request a UI repaint).
    pub fn with_notify(notify: impl Fn() + Send + Sync + 'static) -> Self {
        Self { notify: Some(Arc::new(notify)), ..Self::default() }
    }

    fn update(&self, f: impl FnOnce(&mut ProgressState)) {
        f(&mut self.state.lock().unwrap());
        if let Some(n) = &self.notify {
            n();
        }
    }

    pub fn stage(&self, name: &str, total: usize) {
        self.update(|s| {
            s.stage = name.to_string();
            s.done = 0;
            s.total = total;
        });
    }

    pub fn tick(&self) {
        self.update(|s| s.done += 1);
    }

    pub fn log(&self, msg: impl Into<String>) {
        let msg = msg.into();
        self.update(|s| s.log.push(msg));
    }

    pub fn snapshot(&self) -> ProgressState {
        self.state.lock().unwrap().clone()
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }

    /// Returns an error if the user cancelled; call between units of work.
    pub fn check(&self) -> anyhow::Result<()> {
        if self.is_cancelled() {
            anyhow::bail!("cancelled");
        }
        Ok(())
    }
}
