//! Cooperative cancellation.
//!
//! Cloning a token shares the flag, so a job handed to the render thread and a
//! job handed to the rayon pool can both be stopped from the GUI thread.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::error::{Error, Result};

/// A shared "stop what you are doing" flag.
#[derive(Clone, Debug, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    /// A fresh, un-cancelled token.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A token that can never be cancelled, for callers that do not care.
    #[must_use]
    pub fn never() -> Self {
        Self::default()
    }

    /// Request cancellation. Idempotent, callable from any thread.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    /// Has cancellation been requested?
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    /// `Err(Error::Cancelled)` if cancellation was requested.
    ///
    /// Call this at loop boundaries — per page, per file, per chunk — so long
    /// operations stop promptly without leaving partial output behind.
    pub fn check(&self) -> Result<()> {
        if self.is_cancelled() {
            Err(Error::Cancelled)
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clone_shares_the_flag() {
        let a = CancelToken::new();
        let b = a.clone();
        assert!(a.check().is_ok());
        b.cancel();
        assert!(a.is_cancelled());
        assert!(matches!(a.check(), Err(Error::Cancelled)));
    }
}
