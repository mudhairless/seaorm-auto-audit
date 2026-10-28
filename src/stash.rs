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

#[cfg(test)]
mod tests {
    use super::value_to_json;
    use chrono::{FixedOffset, Local, NaiveDate, TimeZone, Utc};
    use sea_orm::Value;
    use serde_json::json;
    use uuid::Uuid;

    // The match's fallback arm (decimals, database enums, `time` and
    // `jiff` values, big decimals) cannot be constructed in this
    // build: those `Value` variants sit behind dependency features
    // the crate does not enable. It stays a named accept. Every arm
    // that can be constructed is pinned here, because every snapshot
    // on every write path routes through this conversion.

    #[test]
    fn the_conversion_keeps_numbers_and_text_faithful() {
        assert_eq!(value_to_json(Value::Bool(Some(true))), json!(true));
        assert_eq!(
            value_to_json(Value::TinyInt(Some(-5))),
            serde_json::Value::from(-5i64)
        );
        assert_eq!(
            value_to_json(Value::SmallInt(Some(-6))),
            serde_json::Value::from(-6i64)
        );
        assert_eq!(
            value_to_json(Value::Int(Some(7))),
            serde_json::Value::from(7i64)
        );
        assert_eq!(
            value_to_json(Value::BigInt(Some(8))),
            serde_json::Value::from(8i64)
        );
        assert_eq!(
            value_to_json(Value::TinyUnsigned(Some(5))),
            serde_json::Value::from(5u64)
        );
        assert_eq!(
            value_to_json(Value::SmallUnsigned(Some(6))),
            serde_json::Value::from(6u64)
        );
        assert_eq!(
            value_to_json(Value::Unsigned(Some(7))),
            serde_json::Value::from(7u64)
        );
        assert_eq!(
            value_to_json(Value::BigUnsigned(Some(8))),
            serde_json::Value::from(8u64)
        );
        assert_eq!(
            value_to_json(Value::Float(Some(1.5f32))),
            serde_json::Value::from(1.5f64)
        );
        assert_eq!(
            value_to_json(Value::Double(Some(2.5))),
            serde_json::Value::from(2.5f64)
        );
        assert_eq!(
            value_to_json(Value::String(Some("ada".to_owned()))),
            json!("ada")
        );
        assert_eq!(value_to_json(Value::Char(Some('x'))), json!("x"));
        assert_eq!(
            value_to_json(Value::Uuid(Some(Uuid::from_u128(42)))),
            json!("00000000-0000-0000-0000-00000000002a")
        );
    }

    #[test]
    fn the_conversion_turns_non_finite_floats_into_null() {
        assert_eq!(
            value_to_json(Value::Double(Some(f64::NAN))),
            serde_json::Value::Null
        );
        assert_eq!(
            value_to_json(Value::Double(Some(f64::INFINITY))),
            serde_json::Value::Null
        );
        assert_eq!(
            value_to_json(Value::Float(Some(f32::NEG_INFINITY))),
            serde_json::Value::Null
        );
    }

    #[test]
    fn the_conversion_stores_bytes_as_numbers_and_passes_json_through() {
        assert_eq!(
            value_to_json(Value::Bytes(Some(vec![0, 127, 255]))),
            json!([0, 127, 255])
        );
        assert_eq!(
            value_to_json(Value::Json(Some(Box::new(json!({"a": 1}))))),
            json!({"a": 1})
        );
    }

    #[test]
    fn the_conversion_formats_dates_and_times_as_iso_strings() {
        let date = NaiveDate::from_ymd_opt(2026, 10, 28).expect("a real date");
        let moment = date
            .and_hms_nano_opt(3, 4, 5, 123_456_000)
            .expect("a real time");

        assert_eq!(
            value_to_json(Value::ChronoDate(Some(date))),
            json!("2026-10-28")
        );
        assert_eq!(
            value_to_json(Value::ChronoTime(Some(moment.time()))),
            json!("03:04:05.123456")
        );
        assert_eq!(
            value_to_json(Value::ChronoDateTime(Some(moment))),
            json!("2026-10-28T03:04:05.123456")
        );
        assert_eq!(
            value_to_json(Value::ChronoDateTimeUtc(Some(
                Utc.from_utc_datetime(&moment)
            ))),
            json!("2026-10-28T03:04:05.123456+00:00")
        );
        assert_eq!(
            value_to_json(Value::ChronoDateTimeWithTimeZone(Some(
                FixedOffset::east_opt(2 * 3600)
                    .expect("a real offset")
                    .from_local_datetime(&moment)
                    .single()
                    .expect("an unambiguous time")
            ))),
            json!("2026-10-28T03:04:05.123456+02:00")
        );

        // The local offset is the machine's own, so only the moment
        // itself is pinned here.
        let local = value_to_json(Value::ChronoDateTimeLocal(Some(
            Local
                .from_local_datetime(&moment)
                .single()
                .expect("an unambiguous time"),
        )));
        let text = local.as_str().expect("a timestamp string");
        assert!(
            text.starts_with("2026-10-28T03:04:05.123456"),
            "the moment, in local time: {text}"
        );
    }

    #[test]
    fn the_conversion_maps_absent_values_to_null() {
        assert_eq!(value_to_json(Value::Bool(None)), serde_json::Value::Null);
        assert_eq!(value_to_json(Value::Int(None)), serde_json::Value::Null);
        assert_eq!(value_to_json(Value::String(None)), serde_json::Value::Null);
        assert_eq!(value_to_json(Value::Json(None)), serde_json::Value::Null);
    }
}
