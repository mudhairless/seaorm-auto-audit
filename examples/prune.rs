//! Pruning the trail: build a small history for one row, see what a
//! policy selects, then delete it for real.
//!
//! ```console
//! cargo run --example prune
//! ```

use sea_orm::{
    ActiveModelTrait,
    ActiveValue::{Set, Unchanged},
    ConnectionTrait, Database, DbErr, EntityTrait, QueryOrder,
};
use seaorm_auto_audit::{Audits, PruneMode, PrunePolicy, audits, prune, scope};
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
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    seaorm_auto_audit::impl_audit_hooks!(Entity, ActiveModel);

    impl seaorm_auto_audit::Auditable for Entity {}
}

const USERS_DDL: &str = r#"
CREATE TABLE IF NOT EXISTS users (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    email TEXT
);
"#;

/// The DDL of the audit table, the same file the crate ships.
const AUDITS_DDL: &str = include_str!("../sql/setup.sqlite.sql");

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

    // One user, written six times inside scopes: a create and five
    // updates, so the trail holds six rows for her.
    let id = Uuid::new_v4();
    scope(None, async {
        users::ActiveModel {
            id: Set(id),
            name: Set("Ada".to_owned()),
            email: Set(Some("ada@example.com".to_owned())),
        }
        .insert(&db)
        .await?;

        for round in 1..=5 {
            users::ActiveModel {
                id: Unchanged(id),
                email: Set(Some(format!("ada+{round}@example.com"))),
                ..Default::default()
            }
            .update(&db)
            .await?;
        }

        Ok::<(), DbErr>(())
    })
    .await?;

    let total = Audits::find().all(&db).await?.len();
    println!("the trail holds {total} rows for Ada");

    // The policy: keep the two most recent audits per row, prune the
    // rest. A dry run reports the selection and deletes nothing.
    let policy = PrunePolicy::keep_latest(2);

    let selected = prune(&db, &policy, PruneMode::DryRun).await?;
    println!(
        "dry run: {} rows selected, {} still in the table",
        selected.len(),
        Audits::find().all(&db).await?.len()
    );

    let removed = prune(&db, &policy, PruneMode::Delete).await?;
    println!("delete: {} rows removed", removed.len());

    let remaining = Audits::find()
        .order_by_asc(audits::Column::CreatedAt)
        .order_by_asc(audits::Column::Id)
        .all(&db)
        .await?;
    println!("what remains:");
    for row in &remaining {
        println!("  {} {}", row.action, row.auditable_id);
    }

    Ok(())
}
