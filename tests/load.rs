//! Load and stress tests: concurrent scopes, two transactions on one row,
//! the bulk wrappers at scale, and the checker over a large tree.
//!
//! Every test here is `#[ignore]`d: they never run in CI. Run them with
//! `./load_test.sh`, which starts throwaway Postgres and MySQL containers
//! and points `DATABASE_URL` / `MYSQL_DATABASE_URL` at them. Without those
//! variables only the SQLite backend runs. `LOAD_N` overrides the bulk
//! size (default 1000).

mod common;

use std::time::Duration;

use common::{ActiveModel, Entity};
use sea_orm::{
    ActiveModelTrait,
    ActiveValue::{NotSet, Set, Unchanged},
    ColumnTrait, Condition, ConnectionTrait, Database, DatabaseConnection, EntityTrait,
    PaginatorTrait, QueryFilter, TransactionTrait,
};
use seaorm_auto_audit::audits;
use seaorm_auto_audit::scan::Checker;
use seaorm_auto_audit::{Audits, scope};
use tempfile::TempDir;
use uuid::Uuid;

const SQLITE_USERS_DDL: &str = r#"
CREATE TABLE IF NOT EXISTS users (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    email TEXT,
    password_hash TEXT,
    age INTEGER NOT NULL
);
"#;

const POSTGRES_USERS_DDL: &str = r#"
CREATE TABLE IF NOT EXISTS users (
    id uuid PRIMARY KEY,
    name text NOT NULL,
    email text,
    password_hash text,
    age integer NOT NULL
);
"#;

const MYSQL_USERS_DDL: &str = r#"
CREATE TABLE IF NOT EXISTS users (
    id BINARY(16) PRIMARY KEY,
    name TEXT NOT NULL,
    email TEXT,
    password_hash TEXT,
    age INTEGER NOT NULL
);
"#;

struct Backend {
    label: &'static str,
    db: DatabaseConnection,
    users_ddl: &'static str,
    audits_ddl: &'static str,
    _dir: Option<TempDir>,
}

/// Every backend the environment offers: SQLite always, Postgres when
/// `DATABASE_URL` is set, MySQL when `MYSQL_DATABASE_URL` is set.
async fn backends() -> Vec<Backend> {
    let mut out = Vec::new();

    let dir = tempfile::tempdir().expect("a temp dir");
    let url = format!("sqlite://{}?mode=rwc", dir.path().join("load.db").display());
    out.push(Backend {
        label: "sqlite",
        db: Database::connect(&url).await.expect("a sqlite connection"),
        users_ddl: SQLITE_USERS_DDL,
        audits_ddl: include_str!("../sql/setup.sqlite.sql"),
        _dir: Some(dir),
    });

    if let Ok(url) = std::env::var("DATABASE_URL") {
        out.push(Backend {
            label: "postgres",
            db: Database::connect(&url)
                .await
                .expect("a postgres connection"),
            users_ddl: POSTGRES_USERS_DDL,
            audits_ddl: include_str!("../sql/setup.postgres.sql"),
            _dir: None,
        });
    }

    if let Ok(url) = std::env::var("MYSQL_DATABASE_URL") {
        out.push(Backend {
            label: "mysql",
            db: Database::connect(&url).await.expect("a mysql connection"),
            users_ddl: MYSQL_USERS_DDL,
            audits_ddl: include_str!("../sql/setup.mysql.sql"),
            _dir: None,
        });
    }

    out
}

/// Run a DDL script, statement by statement.
async fn apply(db: &DatabaseConnection, ddl: &str) {
    let script = ddl
        .lines()
        .filter(|line| !line.trim_start().starts_with("--"))
        .collect::<Vec<_>>()
        .join("\n");

    for statement in script.split(';') {
        let statement = statement.trim();
        if statement.is_empty() {
            continue;
        }
        db.execute_unprepared(statement)
            .await
            .unwrap_or_else(|error| panic!("`{statement}` failed: {error}"));
    }
}

/// A clean pair of tables for one test on one backend.
async fn reset(backend: &Backend) {
    for table in ["audits", "users"] {
        backend
            .db
            .execute_unprepared(&format!("DROP TABLE IF EXISTS {table}"))
            .await
            .expect("a dropped table");
    }
    apply(&backend.db, backend.users_ddl).await;
    apply(&backend.db, backend.audits_ddl).await;
}

async fn audits_of(db: &DatabaseConnection, id: &Uuid) -> Vec<audits::Model> {
    Audits::find()
        .filter(audits::Column::AuditableId.eq(id.to_string()))
        .all(db)
        .await
        .expect("the audit rows")
}

/// The database tests share the Postgres and MySQL servers, and each one
/// resets the tables, so they must not run at the same time. (SQLite is
/// safe by accident: every test builds its own temp file.) Each database
/// test holds this lock for its whole run.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn bulk_size() -> usize {
    std::env::var("LOAD_N")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(1000)
}

fn a_user(id: Uuid, name: &str) -> ActiveModel {
    ActiveModel {
        id: Set(id),
        name: Set(name.to_owned()),
        email: Set(None),
        password_hash: Set(Some("hash".to_owned())),
        age: Set(36),
    }
}

#[tokio::test]
#[ignore = "load test: run ./load_test.sh"]
async fn concurrent_scopes_keep_their_actors_apart() {
    let _serial = SERIAL.lock().await;
    const TASKS: usize = 8;
    const PER_TASK: usize = 25;

    for backend in backends().await {
        reset(&backend).await;
        let mut actors = Vec::new();
        let mut handles = Vec::new();

        for task in 0..TASKS {
            let actor = Uuid::new_v4();
            actors.push(actor);
            let db = backend.db.clone();
            handles.push(tokio::spawn(async move {
                let txn = db.begin().await.expect("a transaction");
                scope(Some(actor.into()), async {
                    for i in 0..PER_TASK {
                        a_user(Uuid::new_v4(), &format!("task{task}-{i}"))
                            .insert(&txn)
                            .await
                            .expect("an insert");
                    }
                })
                .await;
                txn.commit().await.expect("a commit");
            }));
        }
        for handle in handles {
            handle.await.expect("a finished task");
        }

        let audits = Audits::find()
            .filter(audits::Column::Action.eq("create"))
            .all(&backend.db)
            .await
            .expect("the audit rows");
        assert_eq!(
            audits.len(),
            TASKS * PER_TASK,
            "{}: every write audited exactly once",
            backend.label
        );
        for actor in &actors {
            let own = audits
                .iter()
                .filter(|audit| audit.updated_by_id.as_deref() == Some(actor.to_string().as_str()))
                .count();
            assert_eq!(
                own, PER_TASK,
                "{}: no audit row carries another task's actor",
                backend.label
            );
        }
    }
}

#[tokio::test]
#[ignore = "load test: run ./load_test.sh"]
async fn two_transactions_updating_one_row_fork_or_fail_loudly() {
    let _serial = SERIAL.lock().await;
    for backend in backends().await {
        reset(&backend).await;
        let db = backend.db.clone();
        let id = Uuid::new_v4();
        let actor_a = Uuid::new_v4();
        let actor_b = Uuid::new_v4();

        let txn = db.begin().await.expect("a transaction");
        Box::pin(scope(None, async {
            a_user(id, "v0").insert(&txn).await.expect("the seed");
        }))
        .await;
        txn.commit().await.expect("a commit");

        // Writer A updates inside a transaction that stays open.
        let txn_a = db.begin().await.expect("a transaction");
        scope(Some(actor_a.into()), async {
            ActiveModel {
                id: Unchanged(id),
                name: Set("A".to_owned()),
                email: NotSet,
                password_hash: NotSet,
                age: NotSet,
            }
            .update(&txn_a)
            .await
            .expect("writer A's update");
        })
        .await;

        // Writer B starts from the same committed state and tries to
        // update the same row while A is still uncommitted.
        let db_b = db.clone();
        let b = tokio::spawn(async move {
            let txn_b = db_b.begin().await.expect("a transaction");
            let result = scope(Some(actor_b.into()), async {
                ActiveModel {
                    id: Unchanged(id),
                    name: Set("B".to_owned()),
                    email: NotSet,
                    password_hash: NotSet,
                    age: NotSet,
                }
                .update(&txn_b)
                .await
            })
            .await;
            match result {
                Ok(_) => {
                    txn_b.commit().await.expect("writer B's commit");
                    Ok(())
                }
                Err(error) => Err(error.to_string()),
            }
        });

        // Give B time to do its before-read and reach the write.
        tokio::time::sleep(Duration::from_millis(300)).await;
        txn_a.commit().await.expect("writer A's commit");
        let b_result = tokio::time::timeout(Duration::from_secs(30), b)
            .await
            .expect("writer B should finish")
            .expect("writer B's task");

        let audits = audits_of(&db, &id).await;
        let update_a = audits
            .iter()
            .find(|audit| {
                audit.action == "update"
                    && audit.updated_by_id.as_deref() == Some(actor_a.to_string().as_str())
            })
            .expect("writer A's audit row");
        assert_eq!(update_a.before_content.as_ref().unwrap()["name"], "v0");
        assert_eq!(update_a.after_content.as_ref().unwrap()["name"], "A");

        let final_name = Entity::find_by_id(id)
            .one(&db)
            .await
            .expect("the row")
            .expect("the row exists")
            .name;

        if backend.label == "sqlite" {
            // SQLite takes the third path: the second writer's read waits
            // out the first transaction (the write lock plus the driver's
            // busy timeout), so it reads the committed state and the two
            // rows chain like sequential updates.
            let () = b_result.expect("writer B should succeed on sqlite");
            assert_eq!(final_name, "B", "{}", backend.label);
            let update_b = audits
                .iter()
                .find(|audit| {
                    audit.action == "update"
                        && audit.updated_by_id.as_deref() == Some(actor_b.to_string().as_str())
                })
                .expect("writer B's audit row");
            assert_eq!(
                update_b.before_content.as_ref().unwrap()["name"],
                "A",
                "sqlite: B read the state A committed, so the rows chain"
            );
            assert_eq!(update_b.after_content.as_ref().unwrap()["name"], "B");
            continue;
        }

        match b_result {
            Ok(()) => {
                // The fork: B's row is truthful about what B saw (the
                // state before A committed), so the two update rows do
                // not chain. If this ever starts failing because the
                // rows now chain, the behaviour changed on purpose and
                // the Gotchas note in the README must change with it.
                assert_eq!(final_name, "B", "{}", backend.label);
                let update_b = audits
                    .iter()
                    .find(|audit| {
                        audit.action == "update"
                            && audit.updated_by_id.as_deref() == Some(actor_b.to_string().as_str())
                    })
                    .expect("writer B's audit row");
                assert_eq!(
                    update_b.before_content.as_ref().unwrap()["name"],
                    "v0",
                    "{}: B recorded the state it read, not A's result",
                    backend.label
                );
                assert_eq!(update_b.after_content.as_ref().unwrap()["name"], "B");
            }
            Err(message) => {
                // The loud alternative (SQLite's locked database): no
                // audit row for B, and the row keeps A's value.
                let lowered = message.to_lowercase();
                assert!(
                    lowered.contains("locked") || lowered.contains("busy"),
                    "{}: writer B failed, but not with a locking error: {message}",
                    backend.label
                );
                assert_eq!(final_name, "A", "{}", backend.label);
                assert!(
                    audits.iter().all(|audit| {
                        audit.updated_by_id.as_deref() != Some(actor_b.to_string().as_str())
                    }),
                    "{}: a failed writer leaves no audit row",
                    backend.label
                );
            }
        }
    }
}

#[tokio::test]
#[ignore = "load test: run ./load_test.sh"]
async fn bulk_wrappers_at_scale() {
    let _serial = SERIAL.lock().await;
    let n = bulk_size();

    for backend in backends().await {
        reset(&backend).await;
        let ids: Vec<Uuid> = (0..n).map(|_| Uuid::new_v4()).collect();
        let models: Vec<ActiveModel> = ids.iter().map(|id| a_user(*id, "bulk")).collect();

        let txn = backend.db.begin().await.expect("a transaction");
        Box::pin(scope(None, async {
            seaorm_auto_audit::insert_many::<Entity, _, _>(&txn, models)
                .await
                .expect("the bulk insert");
        }))
        .await;
        txn.commit().await.expect("a commit");
        assert_eq!(
            Entity::find().count(&backend.db).await.expect("a count"),
            n as u64,
            "{}: every row inserted",
            backend.label
        );

        let txn = backend.db.begin().await.expect("a transaction");
        Box::pin(scope(None, async {
            seaorm_auto_audit::update_many::<Entity, _>(
                &txn,
                Condition::all(),
                ActiveModel {
                    id: NotSet,
                    name: NotSet,
                    email: NotSet,
                    password_hash: NotSet,
                    age: Set(37),
                },
            )
            .await
            .expect("the bulk update");
        }))
        .await;
        txn.commit().await.expect("a commit");

        let spot = audits_of(&backend.db, &ids[0]).await;
        let update = spot
            .iter()
            .find(|audit| audit.action == "update")
            .expect("an update audit row");
        assert_eq!(
            update.before_content,
            Some(serde_json::json!({"age": 36})),
            "{}: only the changed column is in the pair",
            backend.label
        );
        assert_eq!(
            update.after_content,
            Some(serde_json::json!({"age": 37})),
            "{}",
            backend.label
        );

        let txn = backend.db.begin().await.expect("a transaction");
        Box::pin(scope(None, async {
            seaorm_auto_audit::delete_many::<Entity, _>(&txn, Condition::all())
                .await
                .expect("the bulk delete");
        }))
        .await;
        txn.commit().await.expect("a commit");

        assert_eq!(
            Entity::find().count(&backend.db).await.expect("a count"),
            0,
            "{}: every row deleted",
            backend.label
        );
        for action in ["create", "update", "delete"] {
            let count = Audits::find()
                .filter(audits::Column::Action.eq(action))
                .count(&backend.db)
                .await
                .expect("a count");
            assert_eq!(
                count, n as u64,
                "{}: one {action} audit row per row",
                backend.label
            );
        }
    }
}

#[test]
#[ignore = "load test: run ./load_test.sh"]
fn the_checker_over_a_large_tree() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let root = dir.path();
    let src = root.join("src");
    std::fs::create_dir_all(src.join("allowed")).expect("the tree");

    // Clean files: audited ActiveModel writes the checker leaves alone.
    for i in 0..290 {
        std::fs::write(
            src.join(format!("clean_{i}.rs")),
            "pub async fn write(db: &DatabaseConnection) { am.insert(db).await.unwrap(); }\n",
        )
        .expect("a file");
    }
    // Five files with a forbidden write each: the only findings.
    for i in 0..5 {
        std::fs::write(
            src.join(format!("bad_{i}.rs")),
            "pub async fn write(db: &DatabaseConnection) { users::Entity::insert(am).exec(db).await.unwrap(); }\n",
        )
        .expect("a file");
    }
    // Decoys: forbidden text in a comment and in a string literal.
    for i in 0..3 {
        std::fs::write(
            src.join(format!("decoy_{i}.rs")),
            "// users::Entity::insert(am) is forbidden, use am.insert(db)\n\
             pub const NOTE: &str = \"Entity::delete_many() skips the hooks\";\n",
        )
        .expect("a file");
    }
    // A forbidden write under an allow-listed directory.
    std::fs::write(
        src.join("allowed").join("legacy.rs"),
        "pub async fn write(db: &DatabaseConnection) { users::Entity::insert(am).exec(db).await.unwrap(); }\n",
    )
    .expect("a file");

    let mut checker = Checker::new();
    checker.allow(src.join("allowed"));
    let violations = checker.scan(root);

    assert_eq!(violations.len(), 5, "exactly the planted findings");
    assert!(
        violations
            .iter()
            .all(|violation| violation.path.to_string_lossy().contains("bad_")),
        "every finding is a planted one: {violations:?}"
    );
}
