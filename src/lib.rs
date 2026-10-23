//! Automatic audit trails for [SeaORM](https://www.sea-ql.org/SeaORM/) entities.
//!
//! Mark a model auditable and every create, update and delete lands in an `audits`
//! table with the before and after state and the user who did it — without each
//! route remembering to write a row.
//!
//! # How it works
//!
//! Three pieces, and only three:
//!
//! 1. **Hooks.** [`ActiveModelBehavior`](sea_orm::ActiveModelBehavior) lifecycle
//!    hooks on the auditable entity, delegating to one shared helper. The helper
//!    receives the same connection handle as the write, so the audit row joins the
//!    write's transaction.
//! 2. **The actor.** A task-local holding *who* is making the change. The actor
//!    is optional: a request with a session carries `Some(user_id)`, a webhook or
//!    a background job carries `None`, and the audit row is written unattributed
//!    rather than skipped. An HTTP request also carries [`RequestMetadata`] —
//!    the client address, URL and user agent — which the middleware captures and
//!    the audit row records.
//! 3. **The stash.** `before_save` cannot see the old row and `after_save` cannot
//!    see it either, so the "before" snapshot is parked on the task-local between
//!    the two hooks.
//!
//! # Quick start
//!
//! ```ignore
//! use sea_orm::entity::prelude::*;
//!
//! // 1. the entity opts in
//! impl seaorm_auto_audit::Auditable for users::Entity {
//!     fn redacted_columns() -> &'static [&'static str] {
//!         &["password_hash"]
//!     }
//! }
//!
//! // 2. the hooks are wired (put this in the entity module)
//! seaorm_auto_audit::impl_audit_hooks!(users::Entity, users::ActiveModel);
//!
//! // 3. the request carries the actor
//! async fn handler(req: Request, next: Next) -> Response {
//!     seaorm_auto_audit::middleware::scope_request(req, next, |req| actor_of(req)).await
//! }
//! ```
//!
//! # Rules
//!
//! * Audited tables are written with ActiveModel methods (`insert`, `update`,
//!   `save`, `delete`) **inside a transaction**. Repository-style writes
//!   (`Entity::insert(am)`, `Entity::update(am)`, `update_without_returning`)
//!   skip SeaORM's hooks entirely. For the bulk methods, use the audited
//!   wrappers [`insert_many`], [`update_many`], [`delete_many`] and
//!   [`delete_by_id`] instead of the raw calls. The `seaorm-auto-audit-check`
//!   binary enforces all of this; see [`scan`].
//! * Every audited write runs inside an audit scope ([`scope`]). The scope
//!   carries the actor and the before-state stash; without it the hooks and
//!   wrappers fail loudly rather than writing a half-empty audit row.
//! * `tokio::spawn` does not inherit task-locals. Code spawned mid-request must
//!   re-scope with [`scope`] or [`scope_with_request`].

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod auditable;
pub mod audits;
pub mod bulk;
pub mod context;
pub mod hooks;
pub mod record;
pub mod redact;
pub mod scan;
mod stash;

#[cfg(feature = "axum")]
pub mod middleware;

pub use async_trait;

pub use auditable::Auditable;
pub use audits::Entity as Audits;
pub use bulk::{delete_by_id, delete_many, insert_many, update_many};
pub use context::{
    AuditContext, NO_SCOPE, RequestMetadata, current_request_metadata, current_user_id, scope,
    scope_with_request,
};
pub use hooks::{after_delete, after_save, before_delete, before_save, write_record};
pub use record::{AuditAction, AuditRecord, UnknownAction};
pub use redact::REDACTED;
