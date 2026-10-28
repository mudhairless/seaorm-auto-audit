//! The soak API: a small axum service over one shared database,
//! with the crate's middleware scoping every request. Run 2 to 3
//! replicas of it; writers pick a replica per operation, so writes
//! land on random processes and connection pools.
//!
//! Every write endpoint runs its SeaORM calls inside one database
//! transaction and commits before answering, so an HTTP 200 from a
//! write endpoint means the write and its audit row committed
//! together. The transact-fail endpoint is the deliberate exception:
//! it writes, rolls back, and answers 500.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::routing::{get, post};
use axum::{Json, Router};
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, Condition, ConnectOptions, ConnectionTrait,
    Database, DatabaseConnection, DbErr, EntityTrait, TransactionTrait,
};
use seaorm_auto_audit::middleware::{scope_request, ClientIpSource};
use seaorm_auto_audit::{record_read, scope_suspended, AuditId};
use seaorm_auto_audit_soak::notes::{self, ActiveModel, Entity};
use seaorm_auto_audit_soak::{audits_ddl, notes_ddl, NoteInput};
use serde::Deserialize;
use serde_json::{json, Value as JsonValue};
use uuid::Uuid;

type ApiResult = Result<(StatusCode, Json<JsonValue>), StatusCode>;

fn server_error(error: DbErr) -> StatusCode {
    eprintln!("database error: {error}");
    StatusCode::INTERNAL_SERVER_ERROR
}

/// The actor of a request, from its `x-actor-id` header. Each writer
/// owns one actor identity for the whole run.
fn actor_of(req: &axum::extract::Request) -> Option<AuditId> {
    req.headers()
        .get("x-actor-id")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<Uuid>().ok())
        .map(Into::into)
}

async fn audit_scope(req: axum::extract::Request, next: Next) -> Response {
    scope_request(req, next, actor_of).await
}

fn note_model(list_name: &str, input: &NoteInput) -> ActiveModel {
    ActiveModel {
        id: Set(Uuid::new_v4()),
        list_name: Set(list_name.to_owned()),
        title: Set(input.title.clone()),
        body: Set(input.body.clone()),
        secret_token: Set(input.secret_token.clone()),
        internal_note: Set(input.internal_note.clone()),
    }
}

#[derive(Deserialize)]
struct CreateBody {
    list_name: String,
    #[serde(flatten)]
    note: NoteInput,
}

async fn create_note(
    State(db): State<DatabaseConnection>,
    Json(body): Json<CreateBody>,
) -> ApiResult {
    let txn = db.begin().await.map_err(server_error)?;
    let model = note_model(&body.list_name, &body.note)
        .insert(&txn)
        .await
        .map_err(server_error)?;
    txn.commit().await.map_err(server_error)?;
    Ok((StatusCode::CREATED, Json(json!({ "id": model.id }))))
}

#[derive(Deserialize)]
struct UpdateBody {
    title: Option<String>,
    body: Option<String>,
    secret_token: Option<String>,
    internal_note: Option<String>,
}

async fn update_note(
    State(db): State<DatabaseConnection>,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateBody>,
) -> ApiResult {
    let mut am = ActiveModel {
        id: sea_orm::ActiveValue::Unchanged(id),
        ..Default::default()
    };
    if let Some(title) = body.title {
        am.title = Set(title);
    }
    if let Some(body) = body.body {
        am.body = Set(Some(body));
    }
    if let Some(secret) = body.secret_token {
        am.secret_token = Set(Some(secret));
    }
    if let Some(note) = body.internal_note {
        am.internal_note = Set(Some(note));
    }

    let txn = db.begin().await.map_err(server_error)?;
    match am.update(&txn).await {
        Ok(_) => {
            txn.commit().await.map_err(server_error)?;
            Ok((StatusCode::OK, Json(json!({ "id": id }))))
        }
        Err(DbErr::RecordNotFound(_)) => Err(StatusCode::NOT_FOUND),
        Err(error) => Err(server_error(error)),
    }
}

async fn delete_note(State(db): State<DatabaseConnection>, Path(id): Path<Uuid>) -> ApiResult {
    let txn = db.begin().await.map_err(server_error)?;
    let result = ActiveModel {
        id: sea_orm::ActiveValue::Unchanged(id),
        ..Default::default()
    }
    .delete(&txn)
    .await
    .map_err(server_error)?;
    txn.commit().await.map_err(server_error)?;
    if result.rows_affected == 0 {
        Err(StatusCode::NOT_FOUND)
    } else {
        Ok((StatusCode::OK, Json(json!({ "id": id }))))
    }
}

#[derive(Deserialize)]
struct BulkCreateBody {
    list_name: String,
    items: Vec<NoteInput>,
}

async fn bulk_create(
    State(db): State<DatabaseConnection>,
    Json(body): Json<BulkCreateBody>,
) -> ApiResult {
    let models = body
        .items
        .iter()
        .map(|item| note_model(&body.list_name, item))
        .collect::<Vec<_>>();
    let txn = db.begin().await.map_err(server_error)?;
    let inserted = seaorm_auto_audit::insert_many::<Entity, _, _>(&txn, models)
        .await
        .map_err(server_error)?;
    txn.commit().await.map_err(server_error)?;
    let ids = inserted.iter().map(|model| model.id).collect::<Vec<_>>();
    Ok((StatusCode::CREATED, Json(json!({ "ids": ids }))))
}

#[derive(Deserialize)]
struct BulkUpdateBody {
    list_name: String,
    body: String,
}

async fn bulk_update(
    State(db): State<DatabaseConnection>,
    Json(body): Json<BulkUpdateBody>,
) -> ApiResult {
    let values = ActiveModel {
        body: Set(Some(body.body)),
        ..Default::default()
    };
    let condition = Condition::all().add(notes::Column::ListName.eq(body.list_name));
    let txn = db.begin().await.map_err(server_error)?;
    let updated = seaorm_auto_audit::update_many::<Entity, _>(&txn, condition, values)
        .await
        .map_err(server_error)?;
    txn.commit().await.map_err(server_error)?;
    Ok((
        StatusCode::OK,
        Json(json!({ "updated": updated.rows_affected })),
    ))
}

#[derive(Deserialize)]
struct BulkDeleteBody {
    list_name: String,
}

async fn bulk_delete(
    State(db): State<DatabaseConnection>,
    Json(body): Json<BulkDeleteBody>,
) -> ApiResult {
    let condition = Condition::all().add(notes::Column::ListName.eq(body.list_name));
    let txn = db.begin().await.map_err(server_error)?;
    let deleted = seaorm_auto_audit::delete_many::<Entity, _>(&txn, condition)
        .await
        .map_err(server_error)?;
    txn.commit().await.map_err(server_error)?;
    Ok((
        StatusCode::OK,
        Json(json!({ "deleted": deleted.rows_affected })),
    ))
}

async fn read_note(State(db): State<DatabaseConnection>, Path(id): Path<Uuid>) -> ApiResult {
    let model = Entity::find_by_id(id)
        .one(&db)
        .await
        .map_err(server_error)?
        .ok_or(StatusCode::NOT_FOUND)?;
    record_read::<Entity, _>(
        &db,
        &model,
        Some("soak read"),
        Some(json!({"title": null, "list_name": null})),
    )
    .await
    .map_err(server_error)?;
    Ok((
        StatusCode::OK,
        Json(json!({
            "id": model.id,
            "list_name": model.list_name,
            "title": model.title,
            "body": model.body,
        })),
    ))
}

#[derive(Deserialize)]
struct TransactFailBody {
    title: String,
}

/// Write inside a transaction, then roll the transaction back. The
/// write and its audit row vanish together; the verifier checks
/// that the attempted values never reach the trail.
async fn transact_fail(
    State(db): State<DatabaseConnection>,
    Path(id): Path<Uuid>,
    Json(body): Json<TransactFailBody>,
) -> ApiResult {
    let txn = db.begin().await.map_err(server_error)?;
    let am = ActiveModel {
        id: sea_orm::ActiveValue::Unchanged(id),
        title: Set(body.title),
        ..Default::default()
    };
    am.update(&txn).await.map_err(server_error)?;
    txn.rollback().await.map_err(server_error)?;
    Err(StatusCode::INTERNAL_SERVER_ERROR)
}

#[derive(Deserialize)]
struct ImportBody {
    list_name: String,
    items: Vec<NoteInput>,
}

/// The seeder path: writes inside `scope_suspended`, so the rows
/// land in `notes` and no audit rows follow them.
async fn import_notes(
    State(db): State<DatabaseConnection>,
    Json(body): Json<ImportBody>,
) -> ApiResult {
    let models = body
        .items
        .iter()
        .map(|item| note_model(&body.list_name, item))
        .collect::<Vec<_>>();
    let inserted = scope_suspended(async {
        let mut out = Vec::with_capacity(models.len());
        for model in models {
            out.push(model.insert(&db).await?);
        }
        Ok::<_, DbErr>(out)
    })
    .await
    .map_err(server_error)?;
    let ids = inserted.iter().map(|model| model.id).collect::<Vec<_>>();
    Ok((StatusCode::CREATED, Json(json!({ "ids": ids }))))
}

async fn healthz() -> &'static str {
    "ok"
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL names the shared soak database");
    let mut options = ConnectOptions::new(url.clone());
    options.max_connections(10);
    let db = Database::connect(options).await?;
    db.execute_unprepared(notes_ddl(&url)).await?;
    db.execute_unprepared(audits_ddl(&url)).await?;

    let app = Router::new()
        .route("/healthz", get(healthz))
        .route("/notes", post(create_note))
        .route("/notes/bulk", post(bulk_create))
        .route("/notes/bulk-update", post(bulk_update))
        .route("/notes/bulk-delete", post(bulk_delete))
        .route(
            "/notes/{id}",
            get(read_note).put(update_note).delete(delete_note),
        )
        .route("/notes/{id}/transact-fail", post(transact_fail))
        .route("/import", post(import_notes))
        .layer(middleware::from_fn(audit_scope))
        .layer(ClientIpSource::RightmostXForwardedFor.into_extension())
        .with_state(db);

    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(8080);
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port)).await?;
    println!("soak api listening on {port}");
    axum::serve(listener, app).await?;
    Ok(())
}
