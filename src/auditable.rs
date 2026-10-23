//! What makes a model auditable.

use sea_orm::{DbErr, EntityTrait, ModelTrait, Value};
use serde::Serialize;
use uuid::Uuid;

/// A model whose creates, updates and deletes are recorded in the `audits`
/// table.
///
/// Implement it for the entity, not the model:
///
/// ```ignore
/// impl seaorm_auto_audit::Auditable for users::Entity {
///     fn redacted_columns() -> &'static [&'static str] {
///         &["password_hash"]
///     }
/// }
/// ```
pub trait Auditable: EntityTrait
where
    Self::Model: Serialize,
{
    /// Columns whose values are never written into an audit record.
    ///
    /// Credential material — password hashes, key hashes — belongs here. A
    /// redacted column is not merely blanked: the audit row says whether it
    /// changed, without saying what it changed to. See [`crate::redact`].
    ///
    /// The default is "nothing is redacted".
    fn redacted_columns() -> &'static [&'static str] {
        &[]
    }

    /// The identifier of a row, as stored in `audits.auditable_id`.
    ///
    /// The default reads the row's primary key, which must be a single `uuid`.
    /// Override it for tables with another kind of key.
    fn audit_id(model: &Self::Model) -> Result<Uuid, DbErr> {
        let values: Vec<Value> = model.get_primary_key_value().iter().cloned().collect();
        match values.as_slice() {
            [Value::Uuid(Some(id))] => Ok(*id),
            _ => Err(DbErr::Custom(format!(
                "seaorm-auto-audit: `{}` needs a single uuid primary key for `auditable_id` — \
                 override `Auditable::audit_id` to map another key",
                Self::default().table_name()
            ))),
        }
    }
}
