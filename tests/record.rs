//! The audit record types an application builds by hand.
//!
//! Nothing inside the crate calls `AuditRecord::new` or parses an action
//! from text: the hooks build records as literals and write actions from
//! the enum. These tests pin the public surface that only applications
//! touch.

use std::str::FromStr;

use seaorm_auto_audit::{AuditAction, AuditId, AuditRecord, UnknownAction};
use serde_json::json;

#[test]
fn a_new_record_carries_only_what_it_was_given() {
    let mut record = AuditRecord::new("users", 42_i64, AuditAction::Update);

    assert_eq!(record.table, "users");
    assert_eq!(record.id, AuditId::from(42_i64));
    assert_eq!(record.action, AuditAction::Update);
    assert_eq!(record.before_content, None);
    assert_eq!(record.after_content, None);
    assert_eq!(record.updated_by_id, None);
    assert_eq!(record.ip_address, None);
    assert_eq!(record.url, None);
    assert_eq!(record.user_agent, None);

    // The fields are public so the caller can fill in what it knows.
    record.before_content = Some(json!({ "name": "Ada" }));
    record.after_content = Some(json!({ "name": "Grace" }));
    record.updated_by_id = Some(AuditId::from(7_i64));

    let mut expected = AuditRecord::new("users", 42_i64, AuditAction::Update);
    expected.before_content = Some(json!({ "name": "Ada" }));
    expected.after_content = Some(json!({ "name": "Grace" }));
    expected.updated_by_id = Some(AuditId::from(7_i64));
    assert_eq!(record, expected);
    assert_ne!(
        record,
        AuditRecord::new("users", 42_i64, AuditAction::Create)
    );
}

#[test]
fn actions_round_trip_through_their_stored_text() {
    for (action, text) in [
        (AuditAction::Create, "create"),
        (AuditAction::Update, "update"),
        (AuditAction::Delete, "delete"),
    ] {
        assert_eq!(action.as_str(), text);
        assert_eq!(action.to_string(), text, "Display matches as_str");
        assert_eq!(AuditAction::from_str(text), Ok(action));
    }
}

#[test]
fn an_unknown_action_is_rejected_with_the_value_named() {
    let error = AuditAction::from_str("merge").expect_err("`merge` is not an action");

    assert_eq!(error, UnknownAction("merge".to_owned()));
    assert_eq!(error.to_string(), "`merge` is not a known audit action");

    // It is a real error type, usable with `?` and error chains.
    let _: &dyn std::error::Error = &error;
}
