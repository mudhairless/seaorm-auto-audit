//! The crate end to end on a throwaway SQLite database: one audited
//! entity, a suspended seed write, a scope holding an insert, a recorded
//! read, an update, a bulk insert and a delete, and the trail printed at
//! the end.
//!
//! ```console
//! cargo run --example basic
//! ```

use sea_orm::entity::prelude::*;
use sea_orm::{
    ActiveModelTrait,
    ActiveValue::{Set, Unchanged},
    ConnectionTrait, Database, DbErr, EntityTrait, QueryOrder, TransactionTrait,
};
use seaorm_auto_audit::{Audits, audits, record_read, scope, scope_suspended};
use serde_json::{Value as Json, json};
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

fn user(name: &str) -> users::ActiveModel {
    users::ActiveModel {
        id: Set(Uuid::new_v4()),
        name: Set(name.to_owned()),
        email: Set(Some(format!("{}@example.com", name.to_lowercase()))),
        password_hash: Set(Some(format!("hash-of-{name}"))),
    }
}

fn show(content: &Option<Json>) -> String {
    match content {
        Some(value) => value.to_string(),
        None => "-".to_owned(),
    }
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

    // A seeder writes before any actor exists, with auditing suspended:
    // the row lands in `users`, and no audit row follows it.
    let seeded: Result<(), DbErr> = scope_suspended(async {
        user("Seed").insert(&db).await?;
        Ok(())
    })
    .await;
    seeded?;

    let actor = Uuid::new_v4();
    let txn = db.begin().await?;
    scope(Some(actor.into()), async {
        let ada = user("Ada").insert(&txn).await?;

        // Opening the record is a read worth logging: what was exposed is
        // named by the fetched keys, and the credential key is stripped.
        record_read::<users::Entity, _>(
            &txn,
            &ada,
            Some("opened the record"),
            Some(json!({"name": null, "email": null, "password_hash": null})),
        )
        .await?;

        users::ActiveModel {
            id: Unchanged(ada.id),
            name: Set("Grace".to_owned()),
            ..Default::default()
        }
        .update(&txn)
        .await?;

        let mut inserted = seaorm_auto_audit::insert_many::<users::Entity, _, _>(
            &txn,
            [user("Hopper"), user("Turing")],
        )
        .await?;
        inserted.remove(0).delete(&txn).await?;

        Ok::<(), DbErr>(())
    })
    .await?;
    txn.commit().await?;

    let trail = Audits::find()
        .order_by_asc(audits::Column::CreatedAt)
        .order_by_asc(audits::Column::Id)
        .all(&db)
        .await?;

    println!(
        "{} audit rows (the seed user's insert is not one of them):",
        trail.len()
    );
    for row in &trail {
        let comment = match &row.comment {
            Some(comment) => format!(" ({comment})"),
            None => String::new(),
        };
        println!(
            "{:<6} {} {} by {}{}",
            row.action,
            row.auditable_table,
            row.auditable_id,
            row.updated_by_id.as_deref().unwrap_or("nobody"),
            comment
        );
        println!(
            "       {} -> {}",
            show(&row.before_content),
            show(&row.after_content)
        );
    }

    if let Some(update) = trail.iter().find(|row| row.action == "update") {
        println!("\nthe update, as a diff:");
        for change in update.modified()? {
            println!(
                "  {}: {} -> {}",
                change.column,
                show(&change.before),
                show(&change.after)
            );
        }
    }

    Ok(())
}
