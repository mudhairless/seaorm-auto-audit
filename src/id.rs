//! The identifier type used for audited rows and actors.
//!
//! An audited row's primary key and the acting user's id can be any type the
//! host application uses — a `uuid`, a `bigint`, a string. Rather than force one
//! Rust type on the whole crate, [`AuditId`] stores the identifier in its
//! canonical text form. Every provided constructor and [`From`] impl takes a
//! concrete type, so the conversion is explicit at the call site and the value
//! stored in `audits.auditable_id` / `audits.updated_by_id` is stable across
//! databases.
//!
//! ```ignore
//! // a uuid key
//! let id = AuditId::from(uuid::Uuid::now_v7());
//! // a bigint key
//! let id = AuditId::from(42_i64);
//! // a string key
//! let id = AuditId::from("user_42");
//! ```

use std::fmt;

use sea_orm::{DbErr, Value};
use uuid::Uuid;

/// The identifier of an audited row or of an acting user.
///
/// The value is kept as text so one crate can audit tables whose keys are
/// `uuid`, integer or string. Construct it from any of those types with
/// [`From`], or from a database [`Value`] with [`AuditId::from_value`].
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AuditId(String);

impl AuditId {
    /// The identifier as text.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Take the identifier's text.
    pub fn into_string(self) -> String {
        self.0
    }

    /// Convert a database value into an identifier.
    ///
    /// This is what [`crate::Auditable::audit_id`] uses by default, so a
    /// single-column primary key of any of SeaORM's common scalar types works
    /// without an override.
    pub fn from_value(value: &Value) -> Result<Self, DbErr> {
        match value {
            Value::Uuid(Some(id)) => Ok(Self::from(*id)),
            Value::String(Some(text)) => Ok(Self(text.clone())),
            Value::BigInt(Some(id)) => Ok(Self(id.to_string())),
            Value::Int(Some(id)) => Ok(Self(id.to_string())),
            Value::SmallInt(Some(id)) => Ok(Self(id.to_string())),
            Value::TinyInt(Some(id)) => Ok(Self(id.to_string())),
            Value::Unsigned(Some(id)) => Ok(Self(id.to_string())),
            other => Err(DbErr::Custom(format!(
                "seaorm-auto-audit: cannot use {other:?} as an audit id — override \
                 `Auditable::audit_id` to map this key"
            ))),
        }
    }
}

impl fmt::Display for AuditId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<Uuid> for AuditId {
    fn from(id: Uuid) -> Self {
        Self(id.to_string())
    }
}

impl From<&Uuid> for AuditId {
    fn from(id: &Uuid) -> Self {
        Self(id.to_string())
    }
}

impl From<String> for AuditId {
    fn from(id: String) -> Self {
        Self(id)
    }
}

impl From<&str> for AuditId {
    fn from(id: &str) -> Self {
        Self(id.to_owned())
    }
}

macro_rules! from_integer {
    ($($type:ty),* $(,)?) => {
        $(
            impl From<$type> for AuditId {
                fn from(id: $type) -> Self {
                    Self(id.to_string())
                }
            }
        )*
    };
}

from_integer!(i8, i16, i32, i64, isize, u8, u16, u32, u64, usize);

impl From<AuditId> for String {
    fn from(id: AuditId) -> Self {
        id.0
    }
}

impl PartialEq<str> for AuditId {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<&str> for AuditId {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

impl PartialEq<Uuid> for AuditId {
    fn eq(&self, other: &Uuid) -> bool {
        self.0 == other.to_string()
    }
}

impl PartialEq<AuditId> for Uuid {
    fn eq(&self, other: &AuditId) -> bool {
        other == self
    }
}
