//! The axum middleware end to end: HTTP requests become audited
//! writes, and the audit rows carry the actor, the client address and
//! the URL of the request that made them.
//!
//! The example needs the crate's `axum` feature:
//!
//! ```console
//! cargo run --example axum --features axum
//! ```

#![cfg(feature = "axum")]

use axum::Router;
use axum::body::Body;
use axum::extract::Request;
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::routing::post;
use axum::{Extension, Json};
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ConnectionTrait, Database, DatabaseConnection, EntityTrait,
    QueryOrder, TransactionTrait,
};
use seaorm_auto_audit::middleware::{ClientIpSource, scope_request};
use seaorm_auto_audit::{AuditId, Audits, audits};
use serde_json::json;
use tower::ServiceExt;
use uuid::Uuid;

mod users {
    use sea_orm::entity::prelude::*;
    use serde::{Deserialize, Serialize};

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "users")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        #[sea_orm(column_type = "Text")]
        pub name: String,
        #[sea_orm(column_type = "Text", nullable)]
        pub email: Option<String>,
        /// Credential material: never stored in an audit row.
        #[sea_orm(column_type = "Text", nullable)]
        pub password_hash: Option<String>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    seaorm_auto_audit::impl_audit_hooks!(Entity, ActiveModel);

    impl seaorm_auto_audit::Auditable for Entity {
        fn redacted_columns() -> &'static [&'static str] {
            &["password_hash"]
        }
    }
}

const USERS_DDL: &str = r#"
CREATE TABLE IF NOT EXISTS users (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    email TEXT,
    password_hash TEXT
);
"#;

/// The DDL of the audit table, the same file the crate ships.
const AUDITS_DDL: &str = include_str!("../sql/setup.sqlite.sql");

/// The actor of a request, from its `x-actor-id` header. A real
/// application reads its session here instead; the shape is the same:
/// the user behind the request, or `None` when there is nobody.
fn actor_of(req: &Request) -> Option<AuditId> {
    req.headers()
        .get("x-actor-id")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<Uuid>().ok())
        .map(Into::into)
}

/// The middleware layer: scope an audit context around every request.
async fn audit_scope(req: Request, next: Next) -> Response {
    scope_request(req, next, actor_of).await
}

/// Create a user. The write is audited because the middleware scoped
/// the request; the handler itself says nothing about auditing.
async fn create_user(
    Extension(db): Extension<DatabaseConnection>,
    req: Request,
) -> Result<(StatusCode, Json<serde_json::Value>), StatusCode> {
    let name = req
        .headers()
        .get("x-name")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("Ada")
        .to_owned();

    let txn = db
        .begin()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let user = users::ActiveModel {
        id: Set(Uuid::new_v4()),
        name: Set(name),
        email: Set(None),
        password_hash: Set(Some("hash".to_owned())),
    }
    .insert(&txn)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    txn.commit()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok((
        StatusCode::CREATED,
        Json(json!({ "id": user.id.to_string() })),
    ))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let url = format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("example.db").display()
    );
    let db = Database::connect(&url).await?;
    db.execute_unprepared(USERS_DDL).await?;
    db.execute_unprepared(AUDITS_DDL).await?;

    let app = Router::new()
        .route("/users", post(create_user))
        .layer(middleware::from_fn(audit_scope))
        .layer(ClientIpSource::RightmostXForwardedFor.into_extension())
        .layer(Extension(db.clone()));

    // Two requests, driven in-process (tower's oneshot) instead of over
    // a socket: one with an actor, one without. The first URL carries a
    // query string on purpose: the audit row records the path only,
    // because query strings carry tokens and reset links.
    let actor = Uuid::new_v4();
    for request in [
        Request::builder()
            .method("POST")
            .uri("/users?source=import")
            .header("x-actor-id", actor.to_string())
            .header("x-name", "Ada")
            .header("x-forwarded-for", "203.0.113.7")
            .body(Body::empty())?,
        Request::builder()
            .method("POST")
            .uri("/users")
            .header("x-name", "Grace")
            .body(Body::empty())?,
    ] {
        let response = app.clone().oneshot(request).await?;
        assert!(response.status().is_success(), "{}", response.status());
    }

    let trail = Audits::find()
        .order_by_asc(audits::Column::CreatedAt)
        .order_by_asc(audits::Column::Id)
        .all(&db)
        .await?;

    println!("{} audit rows from two requests:", trail.len());
    for row in &trail {
        println!(
            "{} {} by {} from {} via {}",
            row.action,
            row.auditable_table,
            row.updated_by_id.as_deref().unwrap_or("nobody"),
            row.ip_address.as_deref().unwrap_or("no address"),
            row.url.as_deref().unwrap_or("no url"),
        );
    }

    Ok(())
}
