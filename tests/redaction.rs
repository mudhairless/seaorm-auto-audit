//! The redaction rules, on their own.

use seaorm_auto_audit::redact::{REDACTED, drop_columns, reconcile};
use serde_json::json;

#[test]
fn a_redacted_column_is_dropped_entirely() {
    let mut snapshot = json!({
        "id": "a-uuid",
        "name": "Ada",
        "password_hash": "a-hash",
    });

    drop_columns(&mut snapshot, &["password_hash"]);

    assert_eq!(
        snapshot,
        json!({"id": "a-uuid", "name": "Ada"}),
        "the column is gone, not blanked"
    );
}

#[test]
fn dropping_a_column_that_is_not_there_is_harmless() {
    let mut snapshot = json!({"name": "Ada"});

    drop_columns(&mut snapshot, &["password_hash", "key_hash"]);

    assert_eq!(snapshot, json!({"name": "Ada"}));
}

#[test]
fn a_snapshot_that_is_not_an_object_is_left_alone() {
    let mut snapshot = json!("not a row");

    drop_columns(&mut snapshot, &["password_hash"]);

    assert_eq!(snapshot, json!("not a row"));
}

#[test]
fn a_changed_column_is_marked_on_both_sides() {
    let mut before = json!({"name": "Ada", "password_hash": "old"});
    let mut after = json!({"name": "Ada", "password_hash": "new"});

    reconcile(&mut before, &mut after, &["password_hash"]);

    assert_eq!(before["password_hash"], json!(REDACTED));
    assert_eq!(after["password_hash"], json!(REDACTED));
}

#[test]
fn an_unchanged_column_is_dropped_from_both_sides() {
    let mut before = json!({"name": "Ada", "password_hash": "same"});
    let mut after = json!({"name": "Grace", "password_hash": "same"});

    reconcile(&mut before, &mut after, &["password_hash"]);

    assert!(before.get("password_hash").is_none());
    assert!(after.get("password_hash").is_none());
    assert_eq!(before["name"], json!("Ada"));
    assert_eq!(after["name"], json!("Grace"));
}

#[test]
fn a_column_that_appears_on_one_side_only_is_a_change() {
    let mut before = json!({"name": "Ada"});
    let mut after = json!({"name": "Ada", "password_hash": "new"});

    reconcile(&mut before, &mut after, &["password_hash"]);

    assert_eq!(before["password_hash"], json!(REDACTED));
    assert_eq!(after["password_hash"], json!(REDACTED));
}

#[test]
fn a_column_that_disappears_is_a_change() {
    let mut before = json!({"name": "Ada", "password_hash": "old"});
    let mut after = json!({"name": "Ada"});

    reconcile(&mut before, &mut after, &["password_hash"]);

    assert_eq!(before["password_hash"], json!(REDACTED));
    assert_eq!(after["password_hash"], json!(REDACTED));
}

#[test]
fn a_null_value_that_stays_null_is_unchanged() {
    let mut before = json!({"name": "Ada", "password_hash": null});
    let mut after = json!({"name": "Grace", "password_hash": null});

    reconcile(&mut before, &mut after, &["password_hash"]);

    assert!(before.get("password_hash").is_none());
    assert!(after.get("password_hash").is_none());
}

#[test]
fn a_null_value_that_becomes_a_value_is_a_change() {
    let mut before = json!({"name": "Ada", "password_hash": null});
    let mut after = json!({"name": "Ada", "password_hash": "new"});

    reconcile(&mut before, &mut after, &["password_hash"]);

    assert_eq!(before["password_hash"], json!(REDACTED));
    assert_eq!(after["password_hash"], json!(REDACTED));
}

#[test]
fn a_value_that_becomes_null_is_a_change() {
    let mut before = json!({"name": "Ada", "password_hash": "old"});
    let mut after = json!({"name": "Ada", "password_hash": null});

    reconcile(&mut before, &mut after, &["password_hash"]);

    assert_eq!(before["password_hash"], json!(REDACTED));
    assert_eq!(after["password_hash"], json!(REDACTED));
}

#[test]
fn several_columns_are_handled_one_by_one() {
    let mut before = json!({
        "name": "Ada",
        "password_hash": "old",
        "key_hash": "same",
        "key_prefix": "ftg_",
    });
    let mut after = json!({
        "name": "Grace",
        "password_hash": "new",
        "key_hash": "same",
        "key_prefix": "ftg_",
    });

    reconcile(&mut before, &mut after, &["password_hash", "key_hash"]);

    // The password moved: marked on both sides.
    assert_eq!(before["password_hash"], json!(REDACTED));
    assert_eq!(after["password_hash"], json!(REDACTED));
    // The key hash did not: dropped from both sides.
    assert!(before.get("key_hash").is_none());
    assert!(after.get("key_hash").is_none());
    // The public half of the key stays: it identifies the key.
    assert_eq!(before["key_prefix"], json!("ftg_"));
}
