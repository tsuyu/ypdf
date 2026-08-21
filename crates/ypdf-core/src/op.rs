//! Operation identifiers.
//!
//! Every user-visible operation gets one. It threads through logs (spec §31),
//! progress events, and error reports so a single line in a JSON log can be tied
//! back to the run that produced it.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::Serialize;

static NEXT: AtomicU64 = AtomicU64::new(1);

/// Process-unique identifier for one operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct OperationId(u64);

impl OperationId {
    /// Allocate the next identifier.
    #[must_use]
    pub fn new() -> Self {
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }

    /// The raw numeric value, for callers that need to store it.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl Default for OperationId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for OperationId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "op-{:08x}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_and_increasing() {
        let a = OperationId::new();
        let b = OperationId::new();
        assert!(b.get() > a.get());
    }

    #[test]
    fn display_is_padded() {
        assert_eq!(OperationId(1).to_string(), "op-00000001");
    }
}
