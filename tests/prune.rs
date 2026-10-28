//! Prune, against a real SQLite database: one fresh file per test, so a
//! global operation over the `audits` table cannot meet another test's rows.

use std::time::Duration;

use sea_orm::entity::prelude::DateTimeWithTimeZone;
use sea_orm::{ActiveModelTrait, ConnectionTrait, Database, DatabaseConnection, EntityTrait, Set};
use seaorm_auto_audit::audits;
use seaorm_auto_audit::{PruneMode, PrunePolicy, prune};
use tempfile::TempDir;
use uuid::Uuid;

/// The DDL of the audit table, the same file the crate ships.
const AUDITS_DDL: &str = include_str!("../sql/setup.sqlite.sql");

async fn fresh_db() -> (TempDir, DatabaseConnection) {
    let dir = tempfile::tempdir().expect("a temp dir");
    let url = format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("prune.db").display()
    );
    let db = Database::connect(&url).await.expect("a connection");
    db.execute_unprepared(AUDITS_DDL)
        .await
        .expect("the audits table");
    (dir, db)
}

async fn insert_audit(db: &DatabaseConnection, auditable_id: &str, created_at: &str) -> Uuid {
    let id = Uuid::now_v7();
    audits::ActiveModel {
        id: Set(id),
        created_at: Set(
            DateTimeWithTimeZone::parse_from_rfc3339(created_at).expect("a fixed timestamp")
        ),
        auditable_table: Set("users".to_owned()),
        auditable_id: Set(auditable_id.to_owned()),
        action: Set("create".to_owned()),
        before_content: Set(None),
        after_content: Set(None),
        updated_by_id: Set(None),
        ip_address: Set(None),
        url: Set(None),
        user_agent: Set(None),
        comment: Set(None),
    }
    .insert(db)
    .await
    .expect("an audit row");
    id
}

async fn audit_count(db: &DatabaseConnection) -> usize {
    audits::Entity::find()
        .all(db)
        .await
        .expect("the audit rows")
        .len()
}

fn ids(rows: &[audits::Model]) -> Vec<Uuid> {
    rows.iter().map(|row| row.id).collect()
}

/// Five audits for one row, oldest first, plus two for another.
async fn five_and_two(db: &DatabaseConnection) -> (Vec<Uuid>, Vec<Uuid>) {
    let mut one = Vec::new();
    for day in 1..=5 {
        one.push(insert_audit(db, "1", &format!("2020-01-0{day}T00:00:00+00:00")).await);
    }
    let mut two = Vec::new();
    for day in 1..=2 {
        two.push(insert_audit(db, "2", &format!("2020-02-0{day}T00:00:00+00:00")).await);
    }
    (one, two)
}

#[tokio::test]
async fn a_dry_run_reports_the_rows_beyond_the_keep_count_and_deletes_nothing() {
    let (_dir, db) = fresh_db().await;
    let (one, _two) = five_and_two(&db).await;

    let reported = prune(&db, &PrunePolicy::keep_latest(2), PruneMode::DryRun)
        .await
        .expect("a dry run");

    assert_eq!(
        ids(&reported),
        one[..3].to_vec(),
        "the three oldest of the five, oldest first"
    );
    assert_eq!(audit_count(&db).await, 7, "a dry run deletes nothing");
}

#[tokio::test]
async fn a_delete_removes_and_reports_the_rows_beyond_the_keep_count() {
    let (_dir, db) = fresh_db().await;
    let (one, _two) = five_and_two(&db).await;

    let removed = prune(&db, &PrunePolicy::keep_latest(2), PruneMode::Delete)
        .await
        .expect("a prune");

    assert_eq!(ids(&removed), one[..3].to_vec());
    assert_eq!(audit_count(&db).await, 4, "two kept per audited row");
}

#[tokio::test]
async fn an_age_policy_removes_only_rows_older_than_the_age() {
    let (_dir, db) = fresh_db().await;
    let ancient = insert_audit(&db, "1", "1990-06-01T00:00:00+00:00").await;
    insert_audit(&db, "1", "2024-01-01T00:00:00+00:00").await;

    // Ten thousand days back from now covers 1990 and not 2024.
    let removed = prune(
        &db,
        &PrunePolicy::older_than(Duration::from_secs(10_000 * 24 * 60 * 60)),
        PruneMode::Delete,
    )
    .await
    .expect("a prune");

    assert_eq!(ids(&removed), vec![ancient]);
    assert_eq!(audit_count(&db).await, 1);
}

#[tokio::test]
async fn the_two_rules_combine_as_a_union() {
    let (_dir, db) = fresh_db().await;
    // Three recent audits for one row: keep one, the count rule takes two.
    let mut recent = Vec::new();
    for day in 1..=3 {
        recent.push(insert_audit(&db, "1", &format!("2024-01-0{day}T00:00:00+00:00")).await);
    }
    // One ancient audit for another row: the count rule would keep it, the
    // age rule takes it.
    let ancient = insert_audit(&db, "2", "1990-06-01T00:00:00+00:00").await;

    let policy = PrunePolicy {
        keep_latest: Some(1),
        older_than: Some(Duration::from_secs(10_000 * 24 * 60 * 60)),
    };
    let removed = prune(&db, &policy, PruneMode::Delete)
        .await
        .expect("a prune");

    let mut expected = vec![ancient, recent[0], recent[1]];
    expected.sort();
    let mut got = ids(&removed);
    got.sort();
    assert_eq!(got, expected);
    assert_eq!(
        audit_count(&db).await,
        1,
        "only the newest recent row stays"
    );
}

#[tokio::test]
async fn keep_latest_zero_removes_everything() {
    let (_dir, db) = fresh_db().await;
    five_and_two(&db).await;

    let removed = prune(&db, &PrunePolicy::keep_latest(0), PruneMode::Delete)
        .await
        .expect("a prune");

    assert_eq!(removed.len(), 7);
    assert_eq!(audit_count(&db).await, 0);
}

#[tokio::test]
async fn a_policy_with_no_rule_is_refused() {
    let (_dir, db) = fresh_db().await;

    let error = prune(&db, &PrunePolicy::default(), PruneMode::DryRun)
        .await
        .expect_err("a policy with no rule must be refused");

    assert!(
        error.to_string().contains("keep_latest"),
        "the error names the missing rule: {error}"
    );
}

#[tokio::test]
async fn delete_at_scale_removes_more_rows_than_one_statement_allows() {
    let (_dir, db) = fresh_db().await;

    // 40,000 rows: past SQLite's 32,766-variable limit for one `is_in`
    // list, so an un-sliced prune delete would fail here. Seeded in
    // 256-row batches, the same chunk size the crate slices by, so the
    // seeding itself never hits the limit either.
    let created_at = DateTimeWithTimeZone::parse_from_rfc3339("2020-01-01T00:00:00+00:00")
        .expect("a fixed timestamp");
    for start in (0..40_000).step_by(256) {
        let end = (start + 256).min(40_000);
        let models: Vec<audits::ActiveModel> = (start..end)
            .map(|_| audits::ActiveModel {
                id: Set(Uuid::now_v7()),
                created_at: Set(created_at),
                auditable_table: Set("users".to_owned()),
                auditable_id: Set("scale".to_owned()),
                action: Set("create".to_owned()),
                before_content: Set(None),
                after_content: Set(None),
                updated_by_id: Set(None),
                ip_address: Set(None),
                url: Set(None),
                user_agent: Set(None),
                comment: Set(None),
            })
            .collect();
        audits::Entity::insert_many(models)
            .exec(&db)
            .await
            .expect("a seed batch");
    }

    let removed = prune(&db, &PrunePolicy::keep_latest(0), PruneMode::Delete)
        .await
        .expect("a prune past the variable limit");

    assert_eq!(removed.len(), 40_000);
    assert_eq!(audit_count(&db).await, 0);
}
