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
    ActiveModelTrait, DbErr, EntityTrait, IdenStatic, Iterable, ModelTrait, PrimaryKeyToColumn,
    Value,
};
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
    {
        Ok(Self::Present {
            id: E::audit_id(model)?,
            snapshot: to_json::<E>(model)?,
        })
    }
}

/// Build the JSON stored in an audit row from a row.
///
/// The object is keyed by database column name: it is built column by
/// column from the entity's columns, not by serializing the model, so
/// the keys are the names `redacted_columns` and `excluded_columns` are
/// matched against, whatever the Rust field names or serde attributes
/// say. (Keying by serde name made a renamed column silently miss its
/// redaction rule and land in the audit table.)
pub(crate) fn to_json<E>(model: &E::Model) -> Result<Json, DbErr>
where
    E: Auditable,
{
    let mut snapshot = serde_json::Map::new();
    for column in E::Column::iter() {
        snapshot.insert(column.as_str().to_owned(), value_to_json(model.get(column)));
    }

    Ok(Json::Object(snapshot))
}

/// Convert one column value into its JSON form.
///
/// The common kinds convert faithfully: numbers stay numbers, text
/// stays text, bytes become an array of numbers (as serde serialized
/// them), dates and timestamps become ISO strings. `sea_orm::Value`
/// has no serde implementation and no JSON conversion of its own, so
/// the kinds without a faithful JSON form (decimals, database enums,
/// `time` and `jiff` values, big decimals) are stored as their debug
/// text rather than failing the write they belong to. A non-finite
/// float (NaN, infinity) has no JSON number and is stored as null;
/// serializing the model used to fail the write on one instead.
fn value_to_json(value: Value) -> Json {
    match value {
        Value::Bool(value) => value.map_or(Json::Null, Json::Bool),
        Value::TinyInt(value) => value.map_or(Json::Null, |v| Json::from(i64::from(v))),
        Value::SmallInt(value) => value.map_or(Json::Null, |v| Json::from(i64::from(v))),
        Value::Int(value) => value.map_or(Json::Null, |v| Json::from(i64::from(v))),
        Value::BigInt(value) => value.map_or(Json::Null, Json::from),
        Value::TinyUnsigned(value) => value.map_or(Json::Null, |v| Json::from(u64::from(v))),
        Value::SmallUnsigned(value) => value.map_or(Json::Null, |v| Json::from(u64::from(v))),
        Value::Unsigned(value) => value.map_or(Json::Null, |v| Json::from(u64::from(v))),
        Value::BigUnsigned(value) => value.map_or(Json::Null, Json::from),
        Value::Float(value) => value.map_or(Json::Null, |v| json_number(f64::from(v))),
        Value::Double(value) => value.map_or(Json::Null, json_number),
        Value::String(value) => value.map_or(Json::Null, Json::String),
        Value::Char(value) => value.map_or(Json::Null, |v| Json::String(v.to_string())),
        Value::Bytes(value) => value.map_or(Json::Null, |bytes| {
            Json::Array(bytes.iter().map(|byte| Json::from(*byte)).collect())
        }),
        Value::Json(value) => value.map_or(Json::Null, |v| *v),
        Value::ChronoDate(value) => value.map_or(Json::Null, |v| Json::String(v.to_string())),
        Value::ChronoTime(value) => value.map_or(Json::Null, |v| Json::String(v.to_string())),
        Value::ChronoDateTime(value) => value.map_or(Json::Null, |v| {
            Json::String(v.format("%Y-%m-%dT%H:%M:%S%.f").to_string())
        }),
        Value::ChronoDateTimeUtc(value) => value.map_or(Json::Null, |v| {
            Json::String(v.format("%Y-%m-%dT%H:%M:%S%.f%:z").to_string())
        }),
        Value::ChronoDateTimeLocal(value) => value.map_or(Json::Null, |v| {
            Json::String(v.format("%Y-%m-%dT%H:%M:%S%.f%:z").to_string())
        }),
        Value::ChronoDateTimeWithTimeZone(value) => value.map_or(Json::Null, |v| {
            Json::String(v.format("%Y-%m-%dT%H:%M:%S%.f%:z").to_string())
        }),
        Value::Uuid(value) => value.map_or(Json::Null, |v| Json::String(v.to_string())),
        other => Json::String(format!("{other:?}")),
    }
}

/// A JSON number for a float, or null when the value is not finite.
fn json_number(value: f64) -> Json {
    serde_json::Number::from_f64(value).map_or(Json::Null, Json::Number)
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
