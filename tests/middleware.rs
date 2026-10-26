//! The HTTP middleware: the actor travels from the request to the audit row.
//!
//! The test needs the `axum` feature of the crate.

#![cfg(feature = "axum")]

mod common;

use axum::Router;
use axum::body::Body;
use axum::extract::Request;
use axum::extract::State;
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::routing::get;
use common::{ActiveModel, insert_bindings};
use sea_orm::{ActiveModelTrait, ActiveValue::Set, DbBackend, MockDatabase, MockExecResult};
use seaorm_auto_audit::middleware::{Actor, ClientIp, ClientIpSource};
use tower::ServiceExt;
use uuid::Uuid;

/// The middleware an application would install: read the actor from the
/// request extensions and scope the audit context around the handler.
async fn audit_scope(req: Request, next: Next) -> Response {
    seaorm_auto_audit::middleware::scope_request_with_extension(req, next).await
}

/// A handler that audits a write and reports the actor it saw.
async fn handler(State(db): State<sea_orm::DatabaseConnection>) -> String {
    let actor = seaorm_auto_audit::current_user_id();

    let id = Uuid::new_v4();
    ActiveModel {
        id: Set(id),
        name: Set("Ada".to_owned()),
        email: Set(None),
        password_hash: Set(None),
        age: Set(36),
    }
    .insert(&db)
    .await
    .expect("the insert should succeed");

    format!("{id} {actor:?}")
}

#[tokio::test]
async fn the_actor_of_the_request_reaches_the_audit_row() {
    let actor = Uuid::new_v4();
    let db = MockDatabase::new(DbBackend::Postgres)
        .append_query_results([[common::user(Uuid::nil())]])
        .append_exec_results([MockExecResult {
            rows_affected: 1,
            ..Default::default()
        }])
        .into_connection();

    let app = Router::new()
        .route("/", get(handler))
        .layer(middleware::from_fn(audit_scope))
        .with_state(db.clone());

    let mut request = Request::builder().uri("/").body(Body::empty()).unwrap();
    request.extensions_mut().insert(Actor::new(actor));

    let response = app
        .oneshot(request)
        .await
        .expect("the request should succeed");
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("the body should be readable");
    let body = String::from_utf8(body.to_vec()).expect("the body should be text");

    assert!(
        body.contains(&actor.to_string()),
        "the handler should see the actor, got: {body}"
    );

    let log = db.into_transaction_log();
    let statement = log.last().unwrap().statements().last().unwrap();
    let bindings = insert_bindings(statement);

    assert_eq!(
        bindings["updated_by_id"],
        sea_orm::Value::String(Some(actor.to_string()))
    );
    assert_eq!(
        bindings["action"],
        sea_orm::Value::String(Some("create".to_owned()))
    );
}

#[tokio::test]
async fn a_request_without_an_actor_is_still_audited() {
    let db = MockDatabase::new(DbBackend::Postgres)
        .append_query_results([[common::user(Uuid::nil())]])
        .append_exec_results([MockExecResult {
            rows_affected: 1,
            ..Default::default()
        }])
        .into_connection();

    let app = Router::new()
        .route("/", get(handler))
        .layer(middleware::from_fn(audit_scope))
        .with_state(db.clone());

    let request = Request::builder().uri("/").body(Body::empty()).unwrap();
    let response = app
        .oneshot(request)
        .await
        .expect("the request should succeed");
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("the body should be readable");
    let body = String::from_utf8(body.to_vec()).expect("the body should be text");

    assert!(
        body.contains("None"),
        "the handler should see no actor, got: {body}"
    );

    let log = db.into_transaction_log();
    let statement = log.last().unwrap().statements().last().unwrap();
    let bindings = insert_bindings(statement);

    // The change is audited; it is just not attributed to anybody.
    assert_eq!(bindings["updated_by_id"], sea_orm::Value::String(None));
}

#[tokio::test]
async fn the_layer_order_puts_the_actor_in_the_scope() {
    let actor = Uuid::new_v4();
    let db = MockDatabase::new(DbBackend::Postgres)
        .append_query_results([[common::user(Uuid::nil())]])
        .append_exec_results([MockExecResult {
            rows_affected: 1,
            ..Default::default()
        }])
        .into_connection();

    // Stands in for a session guard: it resolves the user and leaves it in
    // the request extensions. The guard is added *after* the scope, because
    // axum runs the layer added last first — the guard has to resolve the
    // actor before the scope captures it.
    let app = Router::new()
        .route("/", get(handler))
        .layer(middleware::from_fn(audit_scope))
        .layer(middleware::from_fn(move |mut req: Request, next: Next| {
            let actor = actor;
            async move {
                req.extensions_mut().insert(Actor::new(actor));
                next.run(req).await
            }
        }))
        .with_state(db.clone());

    let request = Request::builder().uri("/").body(Body::empty()).unwrap();
    let response = app
        .oneshot(request)
        .await
        .expect("the request should succeed");
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("the body should be readable");
    let body = String::from_utf8(body.to_vec()).expect("the body should be text");

    assert!(
        body.contains(&actor.to_string()),
        "the handler should see the actor, got: {body}"
    );

    let log = db.into_transaction_log();
    let statement = log.last().unwrap().statements().last().unwrap();
    let bindings = insert_bindings(statement);

    assert_eq!(
        bindings["updated_by_id"],
        sea_orm::Value::String(Some(actor.to_string()))
    );
}

#[tokio::test]
async fn the_request_details_reach_the_audit_row() {
    let db = MockDatabase::new(DbBackend::Postgres)
        .append_query_results([[common::user(Uuid::nil())]])
        .append_exec_results([MockExecResult {
            rows_affected: 1,
            ..Default::default()
        }])
        .into_connection();

    let app = Router::new()
        .route("/things/{id}", get(handler))
        .layer(middleware::from_fn(audit_scope))
        .layer(ClientIpSource::XRealIp.into_extension())
        .with_state(db.clone());

    let mut request = Request::builder()
        .uri("/things/42?tab=history")
        .header("host", "api.example.com")
        .header("x-real-ip", "203.0.113.7")
        .header("x-forwarded-proto", "https")
        .header("x-forwarded-host", "evil.example")
        .header("user-agent", "ftg-test/1.0")
        .body(Body::empty())
        .unwrap();
    // The configured source wins; the local extension is only a fallback.
    request
        .extensions_mut()
        .insert(ClientIp("198.51.100.9".to_owned()));

    let response = app
        .oneshot(request)
        .await
        .expect("the request should succeed");

    assert!(response.status().is_success());

    let log = db.into_transaction_log();
    let statement = log.last().unwrap().statements().last().unwrap();
    let bindings = insert_bindings(statement);

    assert_eq!(
        bindings["ip_address"],
        sea_orm::Value::String(Some("203.0.113.7".to_owned()))
    );
    // The scheme is the one the request arrived on, from x-forwarded-proto.
    // The host still comes from the request's own Host header, never from
    // x-forwarded-host.
    assert_eq!(
        bindings["url"],
        sea_orm::Value::String(Some(
            "https://api.example.com/things/42?tab=history".to_owned()
        ))
    );
    assert_eq!(
        bindings["user_agent"],
        sea_orm::Value::String(Some("ftg-test/1.0".to_owned()))
    );
}

#[tokio::test]
async fn the_local_client_ip_extension_is_a_fallback() {
    let db = MockDatabase::new(DbBackend::Postgres)
        .append_query_results([[common::user(Uuid::nil())]])
        .append_exec_results([MockExecResult {
            rows_affected: 1,
            ..Default::default()
        }])
        .into_connection();

    let app = Router::new()
        .route("/", get(handler))
        .layer(middleware::from_fn(audit_scope))
        .with_state(db.clone());

    let mut request = Request::builder().uri("/").body(Body::empty()).unwrap();
    request
        .extensions_mut()
        .insert(ClientIp("203.0.113.7".to_owned()));

    let response = app
        .oneshot(request)
        .await
        .expect("the request should succeed");

    assert!(response.status().is_success());

    let log = db.into_transaction_log();
    let statement = log.last().unwrap().statements().last().unwrap();
    let bindings = insert_bindings(statement);

    assert_eq!(
        bindings["ip_address"],
        sea_orm::Value::String(Some("203.0.113.7".to_owned()))
    );
}

#[tokio::test]
async fn the_peer_address_is_the_last_resort() {
    let db = MockDatabase::new(DbBackend::Postgres)
        .append_query_results([[common::user(Uuid::nil())]])
        .append_exec_results([MockExecResult {
            rows_affected: 1,
            ..Default::default()
        }])
        .into_connection();

    let app = Router::new()
        .route("/", get(handler))
        .layer(middleware::from_fn(audit_scope))
        .with_state(db.clone());

    let mut request = Request::builder().uri("/").body(Body::empty()).unwrap();
    request
        .extensions_mut()
        .insert(axum::extract::ConnectInfo(std::net::SocketAddr::from((
            [203, 0, 113, 7],
            4000,
        ))));

    let response = app
        .oneshot(request)
        .await
        .expect("the request should succeed");

    assert!(response.status().is_success());

    let log = db.into_transaction_log();
    let statement = log.last().unwrap().statements().last().unwrap();
    let bindings = insert_bindings(statement);

    assert_eq!(
        bindings["ip_address"],
        sea_orm::Value::String(Some("203.0.113.7".to_owned()))
    );
}

#[tokio::test]
async fn a_configured_source_with_no_header_records_no_address() {
    let db = MockDatabase::new(DbBackend::Postgres)
        .append_query_results([[common::user(Uuid::nil())]])
        .append_exec_results([MockExecResult {
            rows_affected: 1,
            ..Default::default()
        }])
        .into_connection();

    let app = Router::new()
        .route("/", get(handler))
        .layer(middleware::from_fn(audit_scope))
        .layer(ClientIpSource::XRealIp.into_extension())
        .with_state(db.clone());

    let mut request = Request::builder().uri("/").body(Body::empty()).unwrap();
    // Even with a peer address available, a configured source that did not
    // resolve must not be replaced by the proxy's address.
    request
        .extensions_mut()
        .insert(axum::extract::ConnectInfo(std::net::SocketAddr::from((
            [203, 0, 113, 7],
            4000,
        ))));

    let response = app
        .oneshot(request)
        .await
        .expect("the request should succeed");

    assert!(response.status().is_success());

    let log = db.into_transaction_log();
    let statement = log.last().unwrap().statements().last().unwrap();
    let bindings = insert_bindings(statement);

    assert_eq!(bindings["ip_address"], sea_orm::Value::String(None));
}

#[tokio::test]
async fn a_configured_source_with_an_invalid_header_records_no_address() {
    let db = MockDatabase::new(DbBackend::Postgres)
        .append_query_results([[common::user(Uuid::nil())]])
        .append_exec_results([MockExecResult {
            rows_affected: 1,
            ..Default::default()
        }])
        .into_connection();

    let app = Router::new()
        .route("/", get(handler))
        .layer(middleware::from_fn(audit_scope))
        .layer(ClientIpSource::XRealIp.into_extension())
        .with_state(db.clone());

    let request = Request::builder()
        .uri("/")
        .header("x-real-ip", "not-an-ip")
        .body(Body::empty())
        .unwrap();

    let response = app
        .oneshot(request)
        .await
        .expect("the request should succeed");

    assert!(response.status().is_success());

    let log = db.into_transaction_log();
    let statement = log.last().unwrap().statements().last().unwrap();
    let bindings = insert_bindings(statement);

    assert_eq!(bindings["ip_address"], sea_orm::Value::String(None));
}

#[tokio::test]
async fn the_middleware_can_resolve_the_actor_itself() {
    let actor = Uuid::new_v4();
    let db = MockDatabase::new(DbBackend::Postgres)
        .append_query_results([[common::user(Uuid::nil())]])
        .append_exec_results([MockExecResult {
            rows_affected: 1,
            ..Default::default()
        }])
        .into_connection();

    async fn audit_scope(req: Request, next: Next) -> Response {
        let actor = req
            .headers()
            .get("x-actor")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| Uuid::parse_str(value).ok())
            .map(Into::into);
        seaorm_auto_audit::middleware::scope_request(req, next, |_| actor.clone()).await
    }

    let app = Router::new()
        .route("/", get(handler))
        .layer(middleware::from_fn(audit_scope))
        .with_state(db.clone());

    let request = Request::builder()
        .uri("/")
        .header("x-actor", actor.to_string())
        .body(Body::empty())
        .unwrap();
    let response = app
        .oneshot(request)
        .await
        .expect("the request should succeed");

    assert!(response.status().is_success());

    let log = db.into_transaction_log();
    let statement = log.last().unwrap().statements().last().unwrap();
    let bindings = insert_bindings(statement);

    assert_eq!(
        bindings["updated_by_id"],
        sea_orm::Value::String(Some(actor.to_string()))
    );
}
