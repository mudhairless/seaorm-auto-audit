//! `audits::Model::modified()`: the per-column changes a stored audit record
//! describes, over hand-built rows in every shape the crate has written.

use sea_orm::entity::prelude::DateTimeWithTimeZone;
use seaorm_auto_audit::audits::Model;
use seaorm_auto_audit::record::ColumnChange;
use serde_json::{Value as Json, json};
use uuid::Uuid;

fn stored(action: &str, before: Option<Json>, after: Option<Json>) -> Model {
    Model {
        id: Uuid::new_v4(),
        created_at: DateTimeWithTimeZone::parse_from_rfc3339("2026-10-27T00:00:00+00:00")
            .expect("a fixed timestamp"),
        auditable_table: "users".to_owned(),
        auditable_id: "42".to_owned(),
        action: action.to_owned(),
        before_content: before,
        after_content: after,
        updated_by_id: None,
        ip_address: None,
        url: None,
        user_agent: None,
    }
}

fn change(column: &str, before: Option<Json>, after: Option<Json>) -> ColumnChange {
    ColumnChange {
        column: column.to_owned(),
        before,
        after,
    }
}

#[test]
fn a_create_reports_every_column_as_new() {
    let row = stored("create", None, Some(json!({"age": 36, "name": "Ada"})));

    assert_eq!(
        row.modified().expect("a create record diffs"),
        vec![
            change("age", None, Some(json!(36))),
            change("name", None, Some(json!("Ada"))),
        ]
    );
}

#[test]
fn a_delete_reports_every_column_as_removed() {
    let row = stored("delete", Some(json!({"age": 36, "name": "Ada"})), None);

    assert_eq!(
        row.modified().expect("a delete record diffs"),
        vec![
            change("age", Some(json!(36)), None),
            change("name", Some(json!("Ada")), None),
        ]
    );
}

#[test]
fn an_update_reports_the_columns_in_its_stored_pair() {
    // The current shape: only the changed columns are stored, and a changed
    // credential is a pair of redaction markers.
    let row = stored(
        "update",
        Some(json!({"name": "Ada", "password_hash": "[redacted]"})),
        Some(json!({"name": "Grace", "password_hash": "[redacted]"})),
    );

    assert_eq!(
        row.modified().expect("an update record diffs"),
        vec![
            change("name", Some(json!("Ada")), Some(json!("Grace"))),
            change(
                "password_hash",
                Some(json!("[redacted]")),
                Some(json!("[redacted]")),
            ),
        ]
    );
}

#[test]
fn an_update_in_the_old_full_snapshot_shape_reports_only_real_changes() {
    // The 0.x shape: both sides hold the whole row. Unchanged columns are
    // filtered by value; the redacted pair survives because the markers mean
    // the credential changed even though the stored values are equal.
    let row = stored(
        "update",
        Some(json!({
            "age": 36,
            "id": 42,
            "name": "Ada",
            "password_hash": "[redacted]",
        })),
        Some(json!({
            "age": 36,
            "id": 42,
            "name": "Grace",
            "password_hash": "[redacted]",
        })),
    );

    assert_eq!(
        row.modified().expect("a full-snapshot update record diffs"),
        vec![
            change("name", Some(json!("Ada")), Some(json!("Grace"))),
            change(
                "password_hash",
                Some(json!("[redacted]")),
                Some(json!("[redacted]")),
            ),
        ]
    );
}

#[test]
fn a_column_present_on_one_side_only_is_a_change() {
    let row = stored(
        "update",
        Some(json!({"name": "Ada"})),
        Some(json!({"email": "ada@example.com", "name": "Ada"})),
    );

    assert_eq!(
        row.modified().expect("an update record diffs"),
        vec![change("email", None, Some(json!("ada@example.com")),)]
    );
}

#[test]
fn an_update_with_no_content_reports_no_changes() {
    let row = stored("update", None, None);

    assert_eq!(row.modified().expect("an empty update diffs"), vec![]);
}

#[test]
fn a_read_record_is_rejected() {
    let row = stored("read", None, Some(json!({"name": "Ada"})));

    assert!(row.modified().is_err());
}

#[test]
fn an_unknown_action_is_rejected() {
    let row = stored("teleported", None, None);

    assert!(row.modified().is_err());
}
