//! The before-state stash.
//!
//! No single hook sees both states of a row: `before_save` gets a partial
//! ActiveModel, `after_save` gets the row the database returned and the old row
//! is gone. So the "before" snapshot is parked here between the two hooks.
//!
//! Both hooks of one write run inside a single `am.update(db)` future on the
//! same task, so the stash needs no locking beyond a plain mutex (never held
//! across an await). The scope ends with the request, so nothing leaks across
//! requests.
//!
//! The stash is *keyed* by table and primary key rather than holding a single
//! slot: one request may save several rows of the same table (a graph save, a
//! loop), and each of them needs its own before-state. A single slot would
//! make the second row's audit row carry the first row's before-state.

use std::collections::HashMap;

use sea_orm::{
    ActiveModelTrait, DbErr, EntityTrait, Iterable, ModelTrait, PrimaryKeyToColumn, Value,
};
use serde::Serialize;
use serde_json::Value as Json;

use crate::auditable::Auditable;
use crate::id::AuditId;

/// Identifies one audited row of one table.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct StashKey {
    table: &'static str,
    primary_key: String,
}

impl StashKey {
    /// The key of the row described by `model`.
    pub(crate) fn for_model<E>(model: &E::Model) -> Result<Self, DbErr>
    where
        E: EntityTrait,
    {
        let values: Vec<Value> = model.get_primary_key_value().iter().cloned().collect();
        Ok(Self::new::<E>(values))
    }

    /// The key of the row described by `am`.
    pub(crate) fn for_active_model<E>(am: &E::ActiveModel) -> Result<Self, DbErr>
    where
        E: EntityTrait,
    {
        let mut values = Vec::new();
        for primary_key in <E::PrimaryKey as Iterable>::iter() {
            let column = primary_key.into_column();
            let value = am.get(column).into_value().ok_or_else(|| {
                DbErr::Custom(format!(
                    "seaorm-auto-audit: `{}` was written without a primary key",
                    E::default().table_name()
                ))
            })?;
            values.push(value);
        }
        Ok(Self::new::<E>(values))
    }

    fn new<E>(values: Vec<Value>) -> Self
    where
        E: EntityTrait,
    {
        Self {
            table: E::default().table_name(),
            primary_key: key_string(&values),
        }
    }
}

/// A stable string for a primary key, used to tell rows apart.
fn key_string(values: &[Value]) -> String {
    values
        .iter()
        .map(|value| format!("{value:?}"))
        .collect::<Vec<_>>()
        .join("|")
}

/// The parked state of one row.
#[derive(Clone, Debug)]
pub(crate) enum Stashed {
    /// The row existed; this is its identifier and its snapshot.
    Present { id: AuditId, snapshot: Json },
    /// The row did not exist when it was looked up.
    Missing,
}

impl Stashed {
    /// Park the state of a row that exists.
    pub(crate) fn present<E>(model: &E::Model) -> Result<Self, DbErr>
    where
        E: Auditable,
        E::Model: Serialize,
    {
        Ok(Self::Present {
            id: E::audit_id(model)?,
            snapshot: to_json::<E>(model)?,
        })
    }
}

/// Serialize a row into the JSON stored in an audit row.
pub(crate) fn to_json<E>(model: &E::Model) -> Result<Json, DbErr>
where
    E: Auditable,
    E::Model: Serialize,
{
    serde_json::to_value(model).map_err(|error| {
        DbErr::Custom(format!(
            "seaorm-auto-audit: could not serialize a row of `{}`: {error}",
            E::default().table_name()
        ))
    })
}

/// The parked before-states of the rows touched by the current scope.
#[derive(Debug, Default)]
pub(crate) struct Stash {
    entries: HashMap<StashKey, Stashed>,
}

impl Stash {
    pub(crate) fn insert(&mut self, key: StashKey, value: Stashed) {
        self.entries.insert(key, value);
    }

    /// Take the parked state of a row, if there is one.
    ///
    /// Taking removes the entry, so a later write in the same scope cannot pick
    /// up a stale snapshot.
    pub(crate) fn remove(&mut self, key: &StashKey) -> Option<Stashed> {
        self.entries.remove(key)
    }
}
