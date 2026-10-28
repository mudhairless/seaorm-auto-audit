//! The actor: who is making the change, and where the "before" state waits.
//!
//! The actor is ambient, exactly like Laravel's `session()` helper: request
//! scoped code reads it without arguments being threaded through the call
//! stack. SeaORM fixes the hook signature, so there is nowhere else to put it.

use std::future::Future;
use std::sync::Mutex;

use sea_orm::DbErr;

use crate::id::AuditId;
use crate::stash::{Stash, StashKey, Stashed};

/// The error message used when an audited write runs outside an audit scope.
pub const NO_SCOPE: &str = "seaorm-auto-audit: no audit scope is active — audited writes must run inside \
                            `seaorm_auto_audit::scope(..)`";

tokio::task_local! {
    /// The ambient audit context.
    static ACTOR: AuditContext;
    /// Whether auditing is suspended for the current scope.
    static SUSPENDED: bool;
}

/// The request details recorded on an audit row.
///
/// All three are optional: a background job has no request at all, and even an
/// HTTP request may lack a user agent or a resolved client address.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RequestMetadata {
    /// The client address, as resolved by the application.
    pub ip_address: Option<String>,
    /// The URL of the request.
    pub url: Option<String>,
    /// The `User-Agent` header of the request.
    pub user_agent: Option<String>,
}

/// The ambient audit context: the acting user, the request it came from, and
/// the parked before-state.
///
/// The context is always scoped — one HTTP middleware layer scopes every
/// request, background jobs scope their own context at startup — so the helper
/// never has to deal with the context being absent. What varies is what is
/// inside it: a request with a session carries `Some(user_id)`, a webhook or a
/// job acting for nobody carries `None`, and only an HTTP request carries
/// [`RequestMetadata`].
pub(crate) struct AuditContext {
    user_id: Option<AuditId>,
    request: Option<RequestMetadata>,
    stash: Mutex<Stash>,
}

impl AuditContext {
    /// A context for `user_id`, the user making the change.
    ///
    /// `None` means "nobody in particular": the change is still audited, it is
    /// just not attributed to a user.
    pub fn new(user_id: Option<AuditId>) -> Self {
        Self::with_request(user_id, None)
    }

    /// A context for `user_id` with the request it came from.
    pub fn with_request(user_id: Option<AuditId>, request: Option<RequestMetadata>) -> Self {
        Self {
            user_id,
            request,
            stash: Mutex::new(Stash::default()),
        }
    }

    /// The user making the change, if there is one.
    pub fn user_id(&self) -> Option<AuditId> {
        self.user_id.clone()
    }

    /// The request the change came from, if it came from one.
    pub fn request(&self) -> Option<&RequestMetadata> {
        self.request.as_ref()
    }

    /// Park the "before" snapshot of a row until the matching `after_save` or
    /// `after_delete` hook pops it.
    pub(crate) fn stash_insert(&self, key: StashKey, value: Stashed) {
        // A poisoned lock means a previous holder panicked mid-update. The
        // stash is a cache of database state that is re-read on every write,
        // so recovering is safe and beats propagating the panic.
        let mut stash = self.stash.lock().unwrap_or_else(|e| e.into_inner());
        stash.insert(key, value);
    }

    /// Take the parked "before" snapshot of a row, if one is parked.
    pub(crate) fn stash_take(&self, key: &StashKey) -> Option<Stashed> {
        let mut stash = self.stash.lock().unwrap_or_else(|e| e.into_inner());
        stash.remove(key)
    }
}

impl std::fmt::Debug for AuditContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuditContext")
            .field("user_id", &self.user_id)
            .finish_non_exhaustive()
    }
}

/// Run `fut` with an audit context in scope.
///
/// `user_id` is the user making the change; `None` for machine-made changes
/// (webhooks, background jobs, the signup bootstrap). The change is audited
/// either way — `None` only means the audit row carries no user.
///
/// ```ignore
/// // A background job acting for nobody.
/// seaorm_auto_audit::scope(None, async { run_job().await }).await;
/// ```
pub async fn scope<F>(user_id: Option<AuditId>, fut: F) -> F::Output
where
    F: Future,
{
    ACTOR.scope(AuditContext::new(user_id), fut).await
}

/// Run `fut` with auditing suspended.
///
/// Writes inside it proceed unaudited. This exists for seeders, imports and
/// backfills. It is deliberately a named scope at the call site rather than a
/// flag.
///
/// Suspension is independent of the actor scope: it works with no scope at
/// all, and inside a normal scope it lasts until the suspended future
/// completes, after which normal auditing resumes in that scope.
///
/// Like the actor scope, suspension is task-local: it does not cross a
/// `tokio::spawn`. Code spawned inside a suspended scope is audited as
/// usual (and fails outside an actor scope), not silently suspended.
pub async fn scope_suspended<F>(fut: F) -> F::Output
where
    F: Future,
{
    SUSPENDED.scope(true, fut).await
}

pub(crate) fn is_suspended() -> bool {
    SUSPENDED.try_with(|s| *s).unwrap_or(false)
}

/// Run `fut` with an audit context that also carries the request details.
///
/// This is [`scope`] plus the request fields — the HTTP middleware uses it, and
/// code that wants the same attribution outside a request can too. `None`
/// request metadata means "not from a request", which is what the audit row
/// records as three NULL columns.
pub async fn scope_with_request<F>(
    user_id: Option<AuditId>,
    request: Option<RequestMetadata>,
    fut: F,
) -> F::Output
where
    F: Future,
{
    ACTOR
        .scope(AuditContext::with_request(user_id, request), fut)
        .await
}

/// The user making the change in the current scope, if there is one.
///
/// Returns `None` outside a scope and for machine-made changes.
pub fn current_user_id() -> Option<AuditId> {
    ACTOR.try_with(|ctx| ctx.user_id()).unwrap_or(None)
}

/// The request details of the current scope, if it came from a request.
///
/// Returns `None` outside a scope and for machine-made changes.
pub fn current_request_metadata() -> Option<RequestMetadata> {
    ACTOR.try_with(|ctx| ctx.request().cloned()).unwrap_or(None)
}

/// Run `f` against the active context.
///
/// Fails when there is no scope: an audited write outside a scope would lose
/// the "before" snapshot, and a half-empty audit row is worse than a loud
/// failure.
pub(crate) fn with_context<R>(f: impl FnOnce(&AuditContext) -> R) -> Result<R, DbErr> {
    ACTOR
        .try_with(f)
        .map_err(|_| DbErr::Custom(NO_SCOPE.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_context_debug_shows_the_actor_and_hides_the_stash() {
        let actor = uuid::Uuid::new_v4();
        let context = AuditContext::new(Some(actor.into()));

        let rendered = format!("{context:?}");
        assert!(rendered.contains(&actor.to_string()), "{rendered}");
        assert!(!rendered.contains("stash"), "{rendered}");
    }
}
