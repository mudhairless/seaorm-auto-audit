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
//!
//! The same principle applies to every column of an update, redacted or not:
//! a value that did not change carries no information about the update, so an
//! update's snapshots hold only the columns that changed. Creates and deletes
//! keep the full row, because there the whole row is the change. An update
//! that changed nothing reconciles to two empty snapshots, and the caller
//! writes no audit row for it.

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

/// Reconcile a before/after pair for storage as an update.
///
/// The redacted columns are handled first, while both raw values are still
/// present: a redacted column whose raw values are equal is dropped from both
/// snapshots, and one whose raw values differ is written as [`REDACTED`] on
/// both sides, so the audit row shows that the column moved without showing
/// what it moved to.
///
/// Every other column is then held to the same standard: a column whose
/// values are equal on both sides did not change, so it is dropped from both.
/// A column present on only one side is a change and stays. What remains in
/// the pair is exactly the change the update made.
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

    let (Some(before_object), Some(after_object)) = (before.as_object(), after.as_object()) else {
        return;
    };
    let unchanged: Vec<String> = before_object
        .iter()
        .filter(|(column, value)| {
            !columns.contains(&column.as_str()) && after_object.get(*column) == Some(*value)
        })
        .map(|(column, _)| column.clone())
        .collect();
    for column in &unchanged {
        remove(before, column);
        remove(after, column);
    }
}

/// Whether a reconciled pair holds no change at all: both snapshots are
/// empty objects. An update that reconciles to this is not written as an
/// audit row.
pub fn holds_no_change(before: &Json, after: &Json) -> bool {
    fn is_empty(snapshot: &Json) -> bool {
        snapshot.as_object().is_some_and(|object| object.is_empty())
    }
    is_empty(before) && is_empty(after)
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
