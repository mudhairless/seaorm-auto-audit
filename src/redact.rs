//! Keeping credential material out of the audit trail.
//!
//! Two rules, and the second one is the interesting one:
//!
//! 1. On create and delete, redacted columns are dropped from the snapshot
//!    outright. The action already says the account was created or removed.
//! 2. On update, the raw values are compared *first*, while both are still in
//!    memory. A redacted column that did not change is dropped from both
//!    snapshots; a redacted column that did change is written as
//!    [`REDACTED`] on both sides.
//!
//! Naive redaction breaks the "it changed" requirement: both snapshots are full
//! rows, so replacing a password hash with a constant on both sides makes a
//! password-only update produce two identical snapshots. The audit row exists
//! but looks like a no-op, and nothing says the password was the thing that
//! moved. With the rules above, a redacted column present in an audit row
//! means it changed, and no audit row ever holds a hash.

use serde_json::Value as Json;

/// The marker written in place of a redacted value that changed.
pub const REDACTED: &str = "[redacted]";

/// Drop the redacted columns from a snapshot.
///
/// Used for creates and deletes, where there is no "before" to compare with.
pub fn drop_columns(snapshot: &mut Json, columns: &[&str]) {
    let Some(object) = snapshot.as_object_mut() else {
        return;
    };
    for column in columns {
        object.remove(*column);
    }
}

/// Reconcile the redacted columns of a before/after pair.
///
/// A column whose raw values are equal is dropped from both snapshots. A column
/// whose raw values differ is written as [`REDACTED`] on both sides, so the
/// audit row shows that the column moved without showing what it moved to.
pub fn reconcile(before: &mut Json, after: &mut Json, columns: &[&str]) {
    for column in columns {
        let before_value = before.get(column).cloned();
        let after_value = after.get(column).cloned();
        match (before_value, after_value) {
            // The column is not in either snapshot: nothing to redact.
            (None, None) => {}
            // The column did not change: it carries no information, drop it.
            (Some(before_value), Some(after_value)) if before_value == after_value => {
                remove(before, column);
                remove(after, column);
            }
            // The column changed: mark it, on both sides.
            _ => {
                set_marker(before, column);
                set_marker(after, column);
            }
        }
    }
}

/// Remove one column from a snapshot, if the snapshot is an object.
fn remove(snapshot: &mut Json, column: &str) {
    if let Some(object) = snapshot.as_object_mut() {
        object.remove(column);
    }
}

/// Write the redaction marker into a snapshot, if the snapshot is an object.
fn set_marker(snapshot: &mut Json, column: &str) {
    if let Some(object) = snapshot.as_object_mut() {
        object.insert(column.to_owned(), Json::String(REDACTED.to_owned()));
    }
}
