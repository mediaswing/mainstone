//! Work done off the drawing thread.
//!
//! Every Graph call and every MariaDB connection goes over the network, and a
//! slow tenant or an unreachable server would otherwise freeze the window for
//! as long as it takes. A [`Task`] runs a closure on its own thread and hands
//! the answer back to whichever pane started it, a frame later; the thread
//! asks egui for a repaint when it finishes, so nothing has to poll.

use std::sync::mpsc::{Receiver, TryRecvError, channel};

/// Every task can fail, and a failure is a sentence for the status bar.
pub type Outcome<T> = Result<T, String>;

pub struct Task<T> {
    rx: Receiver<Outcome<T>>,
    /// What is being done, for the status bar.
    pub label: String,
}

impl<T: Send + 'static> Task<T> {
    pub fn spawn(
        ctx: &egui::Context,
        label: impl Into<String>,
        work: impl FnOnce() -> Outcome<T> + Send + 'static,
    ) -> Self {
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        let label = label.into();
        let name = label.clone();
        log::debug!("task started: {name}");
        std::thread::spawn(move || {
            let started = std::time::Instant::now();
            let result = work();
            let millis = started.elapsed().as_millis();
            match &result {
                Ok(_) => log::debug!("task finished: {name} in {millis} ms"),
                Err(err) => log::debug!("task failed: {name} after {millis} ms: {err}"),
            }
            // The receiving pane may have been replaced in the meantime, in
            // which case nobody wants the answer any more.
            let _ = tx.send(result);
            ctx.request_repaint();
        });
        Self { rx, label }
    }

    /// The answer, once there is one. A thread that panicked counts as an
    /// answer too, so the pane that started it is not left waiting forever.
    pub fn poll(&self) -> Option<Outcome<T>> {
        match self.rx.try_recv() {
            Ok(result) => Some(result),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                log::error!("background task \"{}\" ended without an answer", self.label);
                Some(Err(format!("{} stopped unexpectedly.", self.label)))
            }
        }
    }
}

/// Collect a finished task out of an `Option`, leaving `None` behind.
pub fn take_finished<T: Send + 'static>(slot: &mut Option<Task<T>>) -> Option<Outcome<T>> {
    let result = slot.as_ref()?.poll()?;
    *slot = None;
    Some(result)
}
