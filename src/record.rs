//! What a change looks like on its way into the `audits` table.

use std::fmt;

use serde_json::Value as Json;
use uuid::Uuid;

/// What happened to an audited row.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AuditAction {
    /// The row was created.
    Create,
    /// The row was updated.
    Update,
    /// The row was deleted.
    Delete,
}

impl AuditAction {
    /// The value stored in `audits.action`.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Update => "update",
            Self::Delete => "delete",
        }
    }
}

/// An `audits.action` value that is not one of the three known actions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnknownAction(pub String);

impl fmt::Display for UnknownAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "`{}` is not a known audit action", self.0)
    }
}

impl std::error::Error for UnknownAction {}

impl std::str::FromStr for AuditAction {
    type Err = UnknownAction;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "create" => Ok(Self::Create),
            "update" => Ok(Self::Update),
            "delete" => Ok(Self::Delete),
            other => Err(UnknownAction(other.to_owned())),
        }
    }
}

impl fmt::Display for AuditAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One audit row, before it is written.
///
/// The helper builds these from the hooks; applications can build them by hand
/// for the writes that bypass the hooks (see the coverage note in the README).
#[derive(Clone, Debug, PartialEq)]
pub struct AuditRecord {
    /// The table of the audited row.
    pub table: String,
    /// The primary key of the audited row.
    pub id: Uuid,
    /// What happened.
    pub action: AuditAction,
    /// The audited row before the change. `None` for creates.
    pub before_content: Option<Json>,
    /// The audited row after the change. `None` for deletes.
    pub after_content: Option<Json>,
    /// The user who made the change. `None` for machine-made changes.
    pub updated_by_id: Option<Uuid>,
    /// The client address of the request. `None` for machine-made changes.
    pub ip_address: Option<String>,
    /// The URL of the request. `None` for machine-made changes.
    pub url: Option<String>,
    /// The `User-Agent` of the request. `None` for machine-made changes.
    pub user_agent: Option<String>,
}

impl AuditRecord {
    /// A record with no snapshots, no actor and no request details.
    ///
    /// The fields are public, so a caller writing a bulk row by hand fills in
    /// what it knows:
    ///
    /// ```ignore
    /// let mut record = seaorm_auto_audit::AuditRecord::new(
    ///     "subscriptions",
    ///     id,
    ///     seaorm_auto_audit::AuditAction::Update,
    /// );
    /// record.before_content = Some(before);
    /// record.after_content = Some(after);
    /// record.updated_by_id = seaorm_auto_audit::current_user_id();
    /// ```
    pub fn new(table: impl Into<String>, id: Uuid, action: AuditAction) -> Self {
        Self {
            table: table.into(),
            id,
            action,
            before_content: None,
            after_content: None,
            updated_by_id: None,
            ip_address: None,
            url: None,
            user_agent: None,
        }
    }

    /// Fill the request details from an ambient context, if there is one.
    pub(crate) fn with_request_metadata(mut self, request: Option<&crate::RequestMetadata>) -> Self {
        if let Some(request) = request {
            self.ip_address = request.ip_address.clone();
            self.url = request.url.clone();
            self.user_agent = request.user_agent.clone();
        }
        self
    }
}
