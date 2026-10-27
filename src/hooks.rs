//! The shared helper: one implementation of the audit behaviour, used by every
//! auditable entity.
//!
//! The four functions here mirror SeaORM's four lifecycle hooks. Per-entity
//! hooks call them and nothing else, so a change to the redaction rules or the
//! audit row shape is one edit in one file.
//!
//! ```ignore
//! impl sea_orm::ActiveModelBehavior for users::ActiveModel {
//!     async fn before_save<C: sea_orm::ConnectionTrait>(self, db: &C, insert: bool) -> Result<Self, sea_orm::DbErr> {
//!         seaorm_auto_audit::before_save::<users::Entity>(self, db, insert).await
//!     }
//!     // ...
//! }
//! ```
//!
//! Or let the macro write the four methods for you:
//!
//! ```ignore
//! seaorm_auto_audit::impl_audit_hooks!(users::Entity, users::ActiveModel);
//! ```

use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, Condition, ConnectionTrait, DbErr,
    EntityTrait, Iterable, PrimaryKeyToColumn, QueryFilter,
};
use serde::Serialize;
use serde_json::Value as Json;

use crate::auditable::Auditable;
use crate::audits;
use crate::context::{is_suspended, with_context};
use crate::id::AuditId;
use crate::record::{AuditAction, AuditRecord};
use crate::redact;
use crate::stash::{StashKey, Stashed, to_json};

/// Capture the state of a row before it is written.
///
/// On an insert there is no "before": the row does not exist yet, and the
/// primary key is not even assigned until the database hands it back. On an
/// update the current row is read and parked on the audit scope, because
/// `after_save` only ever sees the new state.
///
/// The extra `SELECT` runs on the same connection handle as the write, so it
/// joins the write's transaction.
pub async fn before_save<E, C>(
    am: E::ActiveModel,
    db: &C,
    insert: bool,
) -> Result<E::ActiveModel, DbErr>
where
    E: Auditable,
    E::Model: Serialize,
    C: ConnectionTrait,
{
    if is_suspended() {
        return Ok(am);
    }

    // Fail before touching the database when there is no scope: an audited
    // write outside a scope cannot produce a complete audit row.
    with_context(|_| ())?;

    if insert {
        // Nothing to capture: there is no "before".
        return Ok(am);
    }

    let key = StashKey::for_active_model::<E>(&am)?;
    let row = fetch_row::<E, C>(&am, db).await?;
    let stashed = match row {
        Some(model) => Stashed::present::<E>(&model)?,
        None => Stashed::Missing,
    };
    with_context(|ctx| ctx.stash_insert(key, stashed))?;

    Ok(am)
}

/// Record a write that just happened.
///
/// On an insert the audit row holds the new state and no "before". On an
/// update it holds the columns that changed, on both sides, with the
/// redacted columns reconciled so that a credential change shows up as a
/// change without showing the credential. An update that changed nothing
/// writes no audit row.
pub async fn after_save<E, C>(model: E::Model, db: &C, insert: bool) -> Result<E::Model, DbErr>
where
    E: Auditable,
    E::Model: Serialize,
    C: ConnectionTrait,
{
    if is_suspended() {
        return Ok(model);
    }

    let action = if insert {
        AuditAction::Create
    } else {
        AuditAction::Update
    };
    let redacted = E::redacted_columns();
    let mut after = to_json::<E>(&model)?;

    let before = if insert {
        redact::drop_columns(&mut after, redacted);
        redact::drop_columns(&mut after, E::excluded_columns());
        None
    } else {
        let key = StashKey::for_model::<E>(&model)?;
        let stashed = with_context(|ctx| ctx.stash_take(&key))?;
        match stashed {
            Some(Stashed::Present { mut snapshot, .. }) => {
                redact::drop_columns(&mut snapshot, E::excluded_columns());
                redact::drop_columns(&mut after, E::excluded_columns());
                redact::reconcile(&mut snapshot, &mut after, redacted);
                if redact::holds_no_change(&snapshot, &after) {
                    // The update set every column to the value it already
                    // had: nothing changed, so there is nothing to audit.
                    return Ok(model);
                }
                Some(snapshot)
            }
            Some(Stashed::Missing) => {
                // The row was already gone, so the update changed nothing and
                // there is nothing to audit.
                tracing::warn!(
                    table = E::default().table_name(),
                    "seaorm-auto-audit: update matched no row, no audit row written"
                );
                return Ok(model);
            }
            None => {
                return Err(DbErr::Custom(format!(
                    "seaorm-auto-audit: no before-state for `{}` — the update did not go through \
                     `seaorm_auto_audit::before_save`",
                    E::default().table_name()
                )));
            }
        }
    };

    let record = build_record(
        E::default().table_name(),
        E::audit_id(&model)?,
        action,
        before,
        Some(after),
    )?;
    write_record(db, record).await?;

    Ok(model)
}

/// Capture the state of a row before it is deleted.
///
/// The snapshot is parked rather than written: writing the audit row here
/// would log a change that then fails to happen if the delete fails.
pub async fn before_delete<E, C>(am: E::ActiveModel, db: &C) -> Result<E::ActiveModel, DbErr>
where
    E: Auditable,
    E::Model: Serialize,
    C: ConnectionTrait,
{
    if is_suspended() {
        return Ok(am);
    }

    // Fail before touching the database when there is no scope.
    with_context(|_| ())?;

    let key = StashKey::for_active_model::<E>(&am)?;
    let row = fetch_row::<E, C>(&am, db).await?;
    let stashed = match row {
        Some(model) => Stashed::present::<E>(&model)?,
        None => Stashed::Missing,
    };
    with_context(|ctx| ctx.stash_insert(key, stashed))?;

    Ok(am)
}

/// Record a delete that just happened.
///
/// The audit row holds the state the row had and no "after".
pub async fn after_delete<E, C>(am: E::ActiveModel, db: &C) -> Result<E::ActiveModel, DbErr>
where
    E: Auditable,
    E::Model: Serialize,
    C: ConnectionTrait,
{
    if is_suspended() {
        return Ok(am);
    }

    let key = StashKey::for_active_model::<E>(&am)?;
    let stashed = with_context(|ctx| ctx.stash_take(&key))?;

    let (id, mut snapshot) = match stashed {
        Some(Stashed::Present { id, snapshot }) => (id, snapshot),
        Some(Stashed::Missing) => {
            // The row was already gone: the delete changed nothing, so there is
            // nothing to audit.
            tracing::warn!(
                table = E::default().table_name(),
                "seaorm-auto-audit: delete matched no row, no audit row written"
            );
            return Ok(am);
        }
        None => {
            return Err(DbErr::Custom(format!(
                "seaorm-auto-audit: no before-state for `{}` — the delete did not go through \
                 `seaorm_auto_audit::before_delete`",
                E::default().table_name()
            )));
        }
    };

    // The action already says the row was removed; the redacted columns would
    // only repeat what the action says.
    redact::drop_columns(&mut snapshot, E::redacted_columns());
    redact::drop_columns(&mut snapshot, E::excluded_columns());

    let record = build_record(
        E::default().table_name(),
        id,
        AuditAction::Delete,
        Some(snapshot),
        None,
    )?;
    write_record(db, record).await?;

    Ok(am)
}

/// Build a record from the ambient scope: the actor and the request details.
///
/// Fails when there is no scope, before the caller touches the database.
pub(crate) fn build_record(
    table: &str,
    id: AuditId,
    action: AuditAction,
    before: Option<Json>,
    after: Option<Json>,
) -> Result<AuditRecord, DbErr> {
    let (updated_by_id, request) = with_context(|ctx| (ctx.user_id(), ctx.request().cloned()))?;

    Ok(AuditRecord {
        table: table.to_owned(),
        id,
        action,
        before_content: before,
        after_content: after,
        updated_by_id,
        ip_address: None,
        url: None,
        user_agent: None,
    }
    .with_request_metadata(request.as_ref()))
}

/// Write one audit row.
///
/// The row goes through the same connection handle as the write it describes,
/// so an audited write inside a transaction commits the write and its audit
/// row together, or neither.
///
/// This is also the entry point for writes that bypass the hooks on purpose —
/// a bulk update that needs its own audit row, for instance. See the coverage
/// note in the README.
pub async fn write_record<C>(db: &C, record: AuditRecord) -> Result<(), DbErr>
where
    C: ConnectionTrait,
{
    // No `RETURNING`: the inserted row is not needed, and asking for it would
    // cost a round trip on every audited write.
    audits::Entity::insert(audit_row(&record))
        .exec_without_returning(db)
        .await?;

    Ok(())
}

/// Write a batch of audit rows in one statement.
///
/// The bulk wrappers use this: a batch touching N rows costs one audit
/// statement instead of N round trips. An empty batch writes nothing. The
/// rows go through the same connection handle as the writes they describe,
/// exactly as with [`write_record`].
pub async fn write_records<C>(db: &C, records: &[AuditRecord]) -> Result<(), DbErr>
where
    C: ConnectionTrait,
{
    if records.is_empty() {
        return Ok(());
    }

    let rows = records.iter().map(audit_row).collect::<Vec<_>>();
    audits::Entity::insert_many(rows)
        .exec_without_returning(db)
        .await?;

    Ok(())
}

/// The `audits` row of one record.
fn audit_row(record: &AuditRecord) -> audits::ActiveModel {
    audits::ActiveModel {
        id: Set(uuid::Uuid::now_v7()),
        created_at: Set(chrono::Utc::now().fixed_offset()),
        auditable_table: Set(record.table.clone()),
        auditable_id: Set(record.id.clone().into_string()),
        action: Set(record.action.as_str().to_owned()),
        before_content: Set(record.before_content.clone()),
        after_content: Set(record.after_content.clone()),
        updated_by_id: Set(record.updated_by_id.clone().map(AuditId::into_string)),
        ip_address: Set(record.ip_address.clone()),
        url: Set(record.url.clone()),
        user_agent: Set(record.user_agent.clone()),
    }
}

/// Read the current row of an active model.
async fn fetch_row<E, C>(am: &E::ActiveModel, db: &C) -> Result<Option<E::Model>, DbErr>
where
    E: Auditable,
    E::Model: Serialize,
    C: ConnectionTrait,
{
    let mut condition = Condition::all();
    for primary_key in <E::PrimaryKey as Iterable>::iter() {
        let column = primary_key.into_column();
        let value = am.get(column).into_value().ok_or_else(|| {
            DbErr::Custom(format!(
                "seaorm-auto-audit: `{}` was written without a primary key",
                E::default().table_name()
            ))
        })?;
        condition = condition.add(column.eq(value));
    }

    E::find().filter(condition).one(db).await
}

/// Wire SeaORM's lifecycle hooks to the helper.
///
/// ```ignore
/// // in the entity module, replacing `impl ActiveModelBehavior for ActiveModel {}`
/// seaorm_auto_audit::impl_audit_hooks!(users::Entity, users::ActiveModel);
/// ```
#[macro_export]
macro_rules! impl_audit_hooks {
    ($entity:ty, $active_model:ty) => {
        #[$crate::async_trait::async_trait]
        impl sea_orm::ActiveModelBehavior for $active_model {
            async fn before_save<C>(self, db: &C, insert: bool) -> Result<Self, sea_orm::DbErr>
            where
                C: sea_orm::ConnectionTrait,
            {
                $crate::before_save::<$entity, _>(self, db, insert).await
            }

            async fn after_save<C>(
                model: <Self::Entity as sea_orm::EntityTrait>::Model,
                db: &C,
                insert: bool,
            ) -> Result<<Self::Entity as sea_orm::EntityTrait>::Model, sea_orm::DbErr>
            where
                C: sea_orm::ConnectionTrait,
            {
                $crate::after_save::<$entity, _>(model, db, insert).await
            }

            async fn before_delete<C>(self, db: &C) -> Result<Self, sea_orm::DbErr>
            where
                C: sea_orm::ConnectionTrait,
            {
                $crate::before_delete::<$entity, _>(self, db).await
            }

            async fn after_delete<C>(self, db: &C) -> Result<Self, sea_orm::DbErr>
            where
                C: sea_orm::ConnectionTrait,
            {
                $crate::after_delete::<$entity, _>(self, db).await
            }
        }
    };
}
