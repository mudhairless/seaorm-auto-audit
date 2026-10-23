//! Audited wrappers for the bulk write methods.
//!
//! SeaORM's lifecycle hooks fire only from `ActiveModel`'s own methods, so
//! `Entity::insert_many`, `Entity::update_many`, `Entity::delete_many` and
//! `Entity::delete_by_id` leave no audit trail on their own. These wrappers
//! are the sanctioned way to use them on an audited table:
//!
//! ```ignore
//! let txn = db.begin().await?;
//! seaorm_auto_audit::insert_many::<users::Entity, _, _>(&txn, models).await?;
//! seaorm_auto_audit::update_many::<users::Entity, _>(&txn, filter, values).await?;
//! seaorm_auto_audit::delete_many::<users::Entity, _>(&txn, filter).await?;
//! seaorm_auto_audit::delete_by_id::<users::Entity, _, _>(&txn, id).await?;
//! txn.commit().await?;
//! ```
//!
//! Each wrapper reads the affected rows as the before state, runs the write on
//! the same connection handle, reads the after state where the write has one,
//! and writes one audit row per affected row through [`write_record`]. The
//! redaction rules are the same as the hook path's.
//!
//! Run them inside the caller's transaction: that is what makes the writes and
//! their audit rows commit or roll back together.
//!
//! The wrappers forbid nothing. The coverage checker still bans the raw calls
//! in application code (`seaorm-auto-audit-check`); these are the replacement.
//!
//! [`write_record`]: crate::write_record

use std::collections::HashMap;

use sea_orm::{
    ColumnTrait, Condition, ConnectionTrait, DbErr, DeleteResult, EntityTrait, IntoActiveModel,
    Iterable, ModelTrait, PrimaryKeyToColumn, PrimaryKeyTrait, QueryFilter, UpdateResult,
};
use sea_orm::sea_query::IntoValueTuple;
use serde::Serialize;
use uuid::Uuid;

use crate::auditable::Auditable;
use crate::context::with_context;
use crate::hooks::{build_record, write_record};
use crate::record::AuditAction;
use crate::redact;
use crate::stash::to_json;

/// Insert many rows and write one `create` audit row for each.
///
/// The rows are read back in the same statement (`RETURNING`), so the audit
/// row holds what the database actually stored.
pub async fn insert_many<E, C, I>(db: &C, models: I) -> Result<Vec<E::Model>, DbErr>
where
    E: Auditable,
    E::Model: Serialize + IntoActiveModel<E::ActiveModel>,
    C: ConnectionTrait,
    I: IntoIterator<Item = E::ActiveModel>,
{
    with_context(|_| ())?;

    let inserted = E::insert_many(models).exec_with_returning(db).await?;

    for model in &inserted {
        let mut after = to_json::<E>(model)?;
        redact::drop_columns(&mut after, E::redacted_columns());
        let record = build_record(
            E::default().table_name(),
            E::audit_id(model)?,
            AuditAction::Create,
            None,
            Some(after),
        )?;
        write_record(db, record).await?;
    }

    Ok(inserted)
}

/// Update every row matching `filter` and audit each one.
///
/// `values` is the `SET` half of the update, the same `ActiveModel` that
/// `Entity::update_many().set(..)` takes. The before state is every row the
/// filter matches; the after state is read back by primary key, so a row that
/// no longer matches the filter is still audited.
pub async fn update_many<E, C>(
    db: &C,
    filter: Condition,
    values: E::ActiveModel,
) -> Result<UpdateResult, DbErr>
where
    E: Auditable,
    E::Model: Serialize,
    C: ConnectionTrait,
{
    with_context(|_| ())?;

    let before_models = E::find().filter(filter.clone()).all(db).await?;
    let result = E::update_many().set(values).filter(filter).exec(db).await?;

    if before_models.is_empty() {
        return Ok(result);
    }

    let mut after_by_id: HashMap<Uuid, E::Model> =
        select_by_primary_keys::<E, C>(db, &before_models)
            .await?
            .into_iter()
            .map(|model| Ok((E::audit_id(&model)?, model)))
            .collect::<Result<_, DbErr>>()?;

    for before_model in &before_models {
        let id = E::audit_id(before_model)?;
        let mut before = to_json::<E>(before_model)?;

        match after_by_id.remove(&id) {
            Some(after_model) => {
                let mut after = to_json::<E>(&after_model)?;
                redact::reconcile(&mut before, &mut after, E::redacted_columns());
                let record = build_record(
                    E::default().table_name(),
                    id,
                    AuditAction::Update,
                    Some(before),
                    Some(after),
                )?;
                write_record(db, record).await?;
            }
            None => {
                // The row stopped existing as part of the update (a trigger,
                // or a delete racing the update). Record what was there.
                redact::drop_columns(&mut before, E::redacted_columns());
                let record = build_record(
                    E::default().table_name(),
                    id,
                    AuditAction::Delete,
                    Some(before),
                    None,
                )?;
                write_record(db, record).await?;
            }
        }
    }

    Ok(result)
}

/// Delete every row matching `filter` and audit each one.
pub async fn delete_many<E, C>(db: &C, filter: Condition) -> Result<DeleteResult, DbErr>
where
    E: Auditable,
    E::Model: Serialize,
    C: ConnectionTrait,
{
    with_context(|_| ())?;

    let before_models = E::find().filter(filter.clone()).all(db).await?;
    let result = E::delete_many().filter(filter).exec(db).await?;

    for model in &before_models {
        let mut before = to_json::<E>(model)?;
        redact::drop_columns(&mut before, E::redacted_columns());
        let record = build_record(
            E::default().table_name(),
            E::audit_id(model)?,
            AuditAction::Delete,
            Some(before),
            None,
        )?;
        write_record(db, record).await?;
    }

    Ok(result)
}

/// Delete one row by primary key and audit it.
///
/// Takes the same argument as `Entity::delete_by_id`: the primary key value,
/// or a tuple for a composite key.
pub async fn delete_by_id<E, C, T>(db: &C, id: T) -> Result<DeleteResult, DbErr>
where
    E: Auditable,
    E::Model: Serialize,
    C: ConnectionTrait,
    T: Into<<E::PrimaryKey as PrimaryKeyTrait>::ValueType>,
{
    let filter = primary_key_condition::<E>(id.into())?;
    delete_many::<E, C>(db, filter).await
}

/// A condition matching the primary key carried by `value`.
fn primary_key_condition<E>(value: <E::PrimaryKey as PrimaryKeyTrait>::ValueType) -> Result<Condition, DbErr>
where
    E: EntityTrait,
{
    let mut condition = Condition::all();
    let mut keys = <E::PrimaryKey as Iterable>::iter();

    for value in value.into_value_tuple() {
        let key = keys.next().ok_or_else(|| {
            DbErr::Custom(format!(
                "seaorm-auto-audit: `{}` got more primary-key values than key columns",
                E::default().table_name()
            ))
        })?;
        condition = condition.add(key.into_column().eq(value));
    }

    Ok(condition)
}

/// The rows of `models`, read back by primary key.
async fn select_by_primary_keys<E, C>(db: &C, models: &[E::Model]) -> Result<Vec<E::Model>, DbErr>
where
    E: EntityTrait,
    C: ConnectionTrait,
{
    if models.is_empty() {
        return Ok(Vec::new());
    }

    // `OR` of one `AND` per row. Never let this collapse to a bare
    // `Condition::any()` with no clauses: that would select the whole table.
    let mut condition = Condition::any();
    for model in models {
        let mut row = Condition::all();
        for primary_key in <E::PrimaryKey as Iterable>::iter() {
            let column = primary_key.into_column();
            row = row.add(column.eq(model.get(column)));
        }
        condition = condition.add(row);
    }

    E::find().filter(condition).all(db).await
}
