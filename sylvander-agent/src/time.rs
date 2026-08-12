//! Monotonic-safe wall-clock helpers used by Agent-owned records.
//!
//! Product Session time belongs to Runtime, but Agent-owned memory and
//! artifact facts still need a bounded Unix timestamp. Centralizing the
//! conversion avoids retaining the Runtime-owned Session module for a generic
//! clock operation.

use std::time::{SystemTime, UNIX_EPOCH};

/// Return the current Unix timestamp in seconds, saturating on conversion.
#[must_use]
pub fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .try_into()
        .unwrap_or(i64::MAX)
}
