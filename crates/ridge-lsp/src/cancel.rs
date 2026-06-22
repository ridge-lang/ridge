//! Cooperative cancellation for workspace-scale queries.
//!
//! tower-lsp wraps every request in `future::abortable` and aborts it when the
//! client sends `$/cancelRequest`. Aborting drops the handler future at its next
//! poll, but a Ridge query does its work as one synchronous, await-free pass over
//! the analysis index, so the abort can only suppress the already-computed
//! response — it cannot stop the CPU work mid-scan.
//!
//! [`Cancel`] closes that gap. A handler that runs its scan on a blocking thread
//! holds a [`CancelOnDrop`] guard across the `.await`; when the request is
//! cancelled the future is dropped, the guard trips the shared flag, and the scan
//! — which polls the flag between modules — bails. The flag is a plain
//! `AtomicBool`, cheap enough to check on every iteration of a hot loop.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// A cooperative-cancellation flag shared between an async request handler and
/// the blocking task running its query.
///
/// Cloning shares the same underlying flag, so a clone handed to a blocking scan
/// observes a cancellation requested through any other clone. The flag only ever
/// moves from "live" to "cancelled"; it is never reset.
#[derive(Clone, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    /// A fresh, un-cancelled flag.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether cancellation has been requested. A workspace-scale scan calls this
    /// between modules and returns early once it reads `true`.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    /// Request cancellation. Idempotent. Called by [`CancelOnDrop`] when the
    /// owning request future is dropped.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

/// Trips its [`Cancel`] when dropped.
///
/// A handler keeps one of these alive across the `.await` on its blocking query.
/// On the normal path it drops after the query returns (a harmless no-op, as the
/// scan has already finished). When tower-lsp aborts the request, the future —
/// and this guard with it — is dropped before the `.await` resolves, which trips
/// the flag so the still-running blocking scan can notice and stop.
pub struct CancelOnDrop(Cancel);

impl CancelOnDrop {
    /// Bind a guard to `cancel`, to be tripped when the guard is dropped.
    #[must_use]
    pub const fn new(cancel: Cancel) -> Self {
        Self(cancel)
    }
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dropping_guard_trips_flag() {
        let cancel = Cancel::new();
        assert!(!cancel.is_cancelled());
        {
            let _guard = CancelOnDrop::new(cancel.clone());
            assert!(!cancel.is_cancelled(), "still live while the guard is held");
        }
        assert!(
            cancel.is_cancelled(),
            "guard drop must request cancellation"
        );
    }

    #[test]
    fn clones_share_one_flag() {
        let a = Cancel::new();
        let b = a.clone();
        a.cancel();
        assert!(
            b.is_cancelled(),
            "a clone observes cancellation on the original"
        );
    }
}
