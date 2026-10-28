//! What makes a model auditable.

use sea_orm::{DbErr, EntityTrait, ModelTrait, Value};

use crate::id::AuditId;

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
pub trait Auditable: EntityTrait {
    /// Columns whose values are never written into an audit record.
    ///
    /// Credential material — password hashes, key hashes — belongs here. A
    /// redacted column is not merely blanked: the audit row says whether it
    /// changed, without saying what it changed to. See [`crate::redact`].
    ///
    /// The default is "nothing is redacted".
    /// The names are database column names: the column's `column_name`,
    /// not the Rust field name and not any serde name. Snapshots are
    /// keyed by column name, and the lists are matched against those
    /// keys.
    fn redacted_columns() -> &'static [&'static str] {
        &[]
    }

    /// Columns that are left out of audit records entirely.
    ///
    /// An excluded column appears in no snapshot and leaves no marker: as
    /// far as the audit trail is concerned it does not exist. Noise belongs
    /// here. A timestamp or counter that changes on every write would
    /// otherwise bury the changes that matter, and an update that touches
    /// only excluded columns writes no audit row at all.
    ///
    /// This is not a weaker form of [`redacted_columns`](Self::redacted_columns):
    /// a redacted column's changes are still recorded, as a marker. A column
    /// listed in both is excluded; absence wins over the marker.
    ///
    /// The default is "nothing is excluded".
    /// The names are database column names: the column's `column_name`,
    /// not the Rust field name and not any serde name. Snapshots are
    /// keyed by column name, and the lists are matched against those
    /// keys.
    fn excluded_columns() -> &'static [&'static str] {
        &[]
    }

    /// The identifier of a row, as stored in `audits.auditable_id`.
    ///
    /// The default reads a single-column primary key of any common scalar type
    /// — `uuid`, integer or string — and stores its canonical text form. See
    /// [`AuditId`]. Override it for composite keys or any key type not covered
    /// by [`AuditId::from_value`].
    fn audit_id(model: &Self::Model) -> Result<AuditId, DbErr> {
        let values: Vec<Value> = model.get_primary_key_value().iter().cloned().collect();
        match values.as_slice() {
            [value] => AuditId::from_value(value),
            _ => Err(DbErr::Custom(format!(
                "seaorm-auto-audit: `{}` has a composite primary key — override \
                 `Auditable::audit_id` to combine its columns",
                Self::default().table_name()
            ))),
        }
    }
}
