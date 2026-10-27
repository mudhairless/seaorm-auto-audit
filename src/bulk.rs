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
//! and writes one audit row per affected row, batched into a single statement
//! through [`write_records`]. The redaction rules are the same as the hook
//! path's.
//!
//! Run them inside the caller's transaction: that is what makes the writes and
//! their audit rows commit or roll back together.
//!
//! The wrappers forbid nothing. The coverage checker still bans the raw calls
//! in application code (`seaorm-auto-audit-check`); these are the replacement.
//!
//! [`write_records`]: crate::write_records

use std::collections::HashMap;

use sea_orm::sea_query::IntoValueTuple;
use sea_orm::{
    ActiveModelTrait, ActiveValue, ColumnTrait, Condition, ConnectionTrait, DbBackend, DbErr,
    DeleteResult, EntityTrait, IntoActiveModel, Iterable, ModelTrait, PrimaryKeyToColumn,
    PrimaryKeyTrait, QueryFilter, UpdateResult, Value,
};
use serde::Serialize;

use crate::auditable::Auditable;
use crate::context::{is_suspended, with_context};
use crate::hooks::{build_record, write_records};
use crate::id::AuditId;
use crate::record::AuditAction;
use crate::redact;
use crate::stash::to_json;

/// Insert many rows and write one `create` audit row for each.
///
/// The rows are read back after the insert, so the audit row holds what the
/// database actually stored. Backends with `RETURNING` (Postgres, SQLite) get
/// the rows in the insert statement; MySQL does not support that, so every
/// model must carry an explicit primary key and the rows are selected back by
/// it.
pub async fn insert_many<E, C, I>(db: &C, models: I) -> Result<Vec<E::Model>, DbErr>
where
    E: Auditable,
    E::Model: Serialize + IntoActiveModel<E::ActiveModel>,
    C: ConnectionTrait,
    I: IntoIterator<Item = E::ActiveModel>,
{
    if !is_suspended() {
        with_context(|_| ())?;
    }

    let models: Vec<E::ActiveModel> = models.into_iter().collect();
    if models.is_empty() {
        return Ok(Vec::new());
    }
    let keys = explicit_keys::<E>(&models)?;

    let inserted = if db.get_database_backend() == DbBackend::MySql {
        let Some(keys) = keys else {
            return Err(DbErr::Custom(format!(
                "seaorm-auto-audit: MySQL cannot generate primary keys for `{}` inside \
                 `insert_many` — set the primary key on every model, or insert them one at a \
                 time inside an audit scope",
                E::default().table_name()
            )));
        };
        E::insert_many(models).exec(db).await?;
        select_by_key_values::<E, C>(db, &keys).await?
    } else {
        E::insert_many(models).exec_with_returning(db).await?
    };

    if is_suspended() {
        return Ok(inserted);
    }

    let mut records = Vec::with_capacity(inserted.len());
    for model in &inserted {
        let mut after = to_json::<E>(model)?;
        redact::drop_columns(&mut after, E::redacted_columns());
        redact::drop_columns(&mut after, E::excluded_columns());
        records.push(build_record(
            E::default().table_name(),
            E::audit_id(model)?,
            AuditAction::Create,
            None,
            Some(after),
        )?);
    }
    write_records(db, &records).await?;

    Ok(inserted)
}

/// Update every row matching `filter` and audit each one.
///
/// `values` is the `SET` half of the update, the same `ActiveModel` that
/// `Entity::update_many().set(..)` takes. The before state is every row the
/// filter matches; the after state is read back by primary key, so a row that
/// no longer matches the filter is still audited.
///
/// The primary key itself cannot be changed this way: the trail is keyed by
/// the row's existing key, so a `values` model that sets a primary key column
/// is rejected before anything runs.
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
    if !is_suspended() {
        with_context(|_| ())?;
    }

    // The read-back below finds the after state under the rows' existing
    // keys. A `values` model that sets a key column would move a row to a
    // new key, the read-back would miss it, and the row would be recorded
    // as deleted when it is alive under the new key. Refuse up front.
    for primary_key in <E::PrimaryKey as Iterable>::iter() {
        if matches!(values.get(primary_key.into_column()), ActiveValue::Set(_)) {
            return Err(DbErr::Custom(format!(
                "seaorm-auto-audit: `update_many` on `{}` cannot change the primary key — \
                 update the row with `am.update(db)` inside a scope instead",
                E::default().table_name()
            )));
        }
    }

    let before_models = if is_suspended() {
        Vec::new()
    } else {
        E::find().filter(filter.clone()).all(db).await?
    };
    let result = E::update_many().set(values).filter(filter).exec(db).await?;

    if is_suspended() {
        return Ok(result);
    }

    if before_models.is_empty() {
        return Ok(result);
    }

    let mut after_by_id: HashMap<AuditId, E::Model> =
        select_by_primary_keys::<E, C>(db, &before_models)
            .await?
            .into_iter()
            .map(|model| Ok((E::audit_id(&model)?, model)))
            .collect::<Result<_, DbErr>>()?;

    let mut records = Vec::with_capacity(before_models.len());
    for before_model in &before_models {
        let id = E::audit_id(before_model)?;
        let mut before = to_json::<E>(before_model)?;

        match after_by_id.remove(&id) {
            Some(after_model) => {
                let mut after = to_json::<E>(&after_model)?;
                redact::drop_columns(&mut before, E::excluded_columns());
                redact::drop_columns(&mut after, E::excluded_columns());
                redact::reconcile(&mut before, &mut after, E::redacted_columns());
                // A row the update left exactly as it was changed nothing:
                // no audit row for it, though other rows may have changed.
                if !redact::holds_no_change(&before, &after) {
                    records.push(build_record(
                        E::default().table_name(),
                        id,
                        AuditAction::Update,
                        Some(before),
                        Some(after),
                    )?);
                }
            }
            None => {
                // The row stopped existing as part of the update (a trigger,
                // or a delete racing the update). Record what was there.
                redact::drop_columns(&mut before, E::redacted_columns());
                redact::drop_columns(&mut before, E::excluded_columns());
                records.push(build_record(
                    E::default().table_name(),
                    id,
                    AuditAction::Delete,
                    Some(before),
                    None,
                )?);
            }
        }
    }
    write_records(db, &records).await?;

    Ok(result)
}

/// Delete every row matching `filter` and audit each one.
pub async fn delete_many<E, C>(db: &C, filter: Condition) -> Result<DeleteResult, DbErr>
where
    E: Auditable,
    E::Model: Serialize,
    C: ConnectionTrait,
{
    if !is_suspended() {
        with_context(|_| ())?;
    }

    let before_models = if is_suspended() {
        Vec::new()
    } else {
        E::find().filter(filter.clone()).all(db).await?
    };
    let result = E::delete_many().filter(filter).exec(db).await?;

    if is_suspended() {
        return Ok(result);
    }

    let mut records = Vec::with_capacity(before_models.len());
    for model in &before_models {
        let mut before = to_json::<E>(model)?;
        redact::drop_columns(&mut before, E::redacted_columns());
        redact::drop_columns(&mut before, E::excluded_columns());
        records.push(build_record(
            E::default().table_name(),
            E::audit_id(model)?,
            AuditAction::Delete,
            Some(before),
            None,
        )?);
    }
    write_records(db, &records).await?;

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
fn primary_key_condition<E>(
    value: <E::PrimaryKey as PrimaryKeyTrait>::ValueType,
) -> Result<Condition, DbErr>
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

/// The primary key values of every model, or `None` when any model leaves a
/// key to the database.
fn explicit_keys<E>(models: &[E::ActiveModel]) -> Result<Option<Vec<Vec<Value>>>, DbErr>
where
    E: EntityTrait,
{
    let mut keys = Vec::with_capacity(models.len());
    for am in models {
        let mut row = Vec::new();
        for primary_key in <E::PrimaryKey as Iterable>::iter() {
            match am.get(primary_key.into_column()).into_value() {
                Some(value) => row.push(value),
                None => return Ok(None),
            }
        }
        keys.push(row);
    }
    Ok(Some(keys))
}

/// How many key rows one read-back query takes. The condition is an `OR`
/// of one `AND` per row, so its expression tree grows with the row count,
/// and SQLite refuses an expression tree deeper than 1000: a bulk
/// operation over more rows than that failed there with "Expression tree
/// is too large". The read-backs below go in slices of this many keys.
const KEY_CHUNK: usize = 256;

/// The rows with the given primary key values.
async fn select_by_key_values<E, C>(db: &C, keys: &[Vec<Value>]) -> Result<Vec<E::Model>, DbErr>
where
    E: EntityTrait,
    C: ConnectionTrait,
{
    let mut rows = Vec::new();
    for keys in keys.chunks(KEY_CHUNK) {
        let mut condition = Condition::any();
        for values in keys {
            let mut row = Condition::all();
            for (primary_key, value) in <E::PrimaryKey as Iterable>::iter().zip(values.iter()) {
                row = row.add(primary_key.into_column().eq(value.clone()));
            }
            condition = condition.add(row);
        }
        rows.extend(E::find().filter(condition).all(db).await?);
    }

    Ok(rows)
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

    // `OR` of one `AND` per row, in slices (see KEY_CHUNK). Never let a
    // slice collapse to a bare `Condition::any()` with no clauses: that
    // would select the whole table.
    let mut rows = Vec::new();
    for models in models.chunks(KEY_CHUNK) {
        let mut condition = Condition::any();
        for model in models {
            let mut row = Condition::all();
            for primary_key in <E::PrimaryKey as Iterable>::iter() {
                let column = primary_key.into_column();
                row = row.add(column.eq(model.get(column)));
            }
            condition = condition.add(row);
        }
        rows.extend(E::find().filter(condition).all(db).await?);
    }

    Ok(rows)
}
