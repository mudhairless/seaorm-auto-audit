//! Middleware helpers for HTTP requests.
//!
//! The actor is ambient, so the only thing an HTTP layer has to do is say who
//! is acting and wrap the request in an audit scope. Two ways to do that:
//!
//! ```ignore
//! // the app resolves the actor itself
//! async fn audit_scope(State(state): State<AppState>, req: Request, next: Next) -> Response {
//!     seaorm_auto_audit::middleware::scope_request(req, next, |req| state.actor_of(req)).await
//! }
//!
//! // or the app puts the actor in the request extensions
//! async fn audit_scope(req: Request, next: Next) -> Response {
//!     seaorm_auto_audit::middleware::scope_request_with_extension(req, next).await
//! }
//! ```
//!
//! A request with no session behind it (a Stripe webhook, the signup
//! bootstrap) resolves to no actor. That is a valid flow: the change is still
//! audited, it is just not attributed to a user.
//!
//! The middleware also captures the request details an audit row records: the
//! client address, the URL and the `User-Agent`. The address comes from a
//! [`ClientIp`] extension when the application inserted one, and otherwise
//! from the connection's peer address ([`ConnectInfo`]). The crate does not
//! trust a forwarding header on its own — a header is only as trustworthy as
//! the proxy that sets it, so that call belongs to the application.

use std::net::SocketAddr;

use axum::extract::{ConnectInfo, OriginalUri, Request};
use axum::http::{HeaderMap, Uri, header};
use axum::middleware::Next;
use axum::response::Response;
use uuid::Uuid;

use crate::context::{RequestMetadata, scope_with_request};

/// The user making the change, carried in the request extensions.
///
/// The authentication middleware puts one of these in the request; the audit
/// middleware reads it back and scopes the audit context around the handler.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Actor(pub Uuid);

/// The client address, carried in the request extensions.
///
/// Set this from the header a trusted proxy sets (nginx's `X-Real-IP`, for
/// instance) when the application runs behind one. Without it the middleware
/// falls back to the connection's peer address, which behind a proxy is the
/// proxy's address.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientIp(pub String);

/// Scope an audit context around a request, resolving the actor with `resolve`.
///
/// `resolve` is called with the request before the handler runs and returns the
/// user making the change, or `None` when the request has nobody behind it.
pub async fn scope_request(
    req: Request,
    next: Next,
    resolve: impl Fn(&Request) -> Option<Uuid>,
) -> Response {
    let user_id = resolve(&req);
    let request = metadata_of(&req);
    scope_with_request(user_id, Some(request), next.run(req)).await
}

/// Scope an audit context around a request, taking the actor from the request
/// extensions.
///
/// See [`Actor`].
pub async fn scope_request_with_extension(req: Request, next: Next) -> Response {
    let user_id = req.extensions().get::<Actor>().map(|actor| actor.0);
    let request = metadata_of(&req);
    scope_with_request(user_id, Some(request), next.run(req)).await
}

/// The request details recorded on the audit rows of this request.
fn metadata_of(req: &Request) -> RequestMetadata {
    RequestMetadata {
        ip_address: client_ip(req),
        url: Some(url_of(req)),
        user_agent: req
            .headers()
            .get(header::USER_AGENT)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned),
    }
}

/// The client address: what the application supplied, else the peer address.
fn client_ip(req: &Request) -> Option<String> {
    req.extensions()
        .get::<ClientIp>()
        .map(|ip| ip.0.clone())
        .or_else(|| {
            req.extensions()
                .get::<ConnectInfo<SocketAddr>>()
                .map(|info| info.0.ip().to_string())
        })
}

/// The request URL, made absolute when the host is known.
fn url_of(req: &Request) -> String {
    let uri = req
        .extensions()
        .get::<OriginalUri>()
        .map(|original| &original.0)
        .unwrap_or_else(|| req.uri());

    absolute_url(uri, req.headers())
}

/// `path?query` -> `scheme://host/path?query`, when a host header exists.
fn absolute_url(uri: &Uri, headers: &HeaderMap) -> String {
    if uri.scheme().is_some() {
        return uri.to_string();
    }

    let header_value = |name: &str| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .filter(|value| !value.is_empty())
    };
    let scheme = header_value("x-forwarded-proto").unwrap_or("http");
    let host = header_value("x-forwarded-host").or_else(|| header_value("host"));

    match host {
        Some(host) => format!("{scheme}://{host}{uri}"),
        None => uri.to_string(),
    }
}
