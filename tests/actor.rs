//! The actor: the request-level variable that travels on the task-local.
//!
//! The actor is ambient — request-scoped code reads it without arguments
//! being threaded through the call stack — so these tests are about what the
//! task-local does and does not carry.

mod common;

use common::{ActiveModel, insert_bindings, user};
use seaorm_auto_audit::{current_user_id, scope};
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, DbBackend, DbErr, MockDatabase, MockExecResult, Value,
};
use uuid::Uuid;

#[tokio::test]
async fn the_actor_is_read_from_the_task_local() {
    let actor = Uuid::new_v4();

    scope(Some(actor), async {
        assert_eq!(current_user_id(), Some(actor));
    })
    .await;
}

#[tokio::test]
async fn outside_a_scope_there_is_no_actor() {
    assert_eq!(current_user_id(), None);
}

#[tokio::test]
async fn a_scope_without_a_user_has_no_actor() {
    scope(None, async {
        assert_eq!(current_user_id(), None);
    })
    .await;
}

#[tokio::test]
async fn the_inner_scope_wins() {
    let outer = Uuid::new_v4();
    let inner = Uuid::new_v4();

    scope(Some(outer), async {
        assert_eq!(current_user_id(), Some(outer));

        scope(Some(inner), async {
            assert_eq!(current_user_id(), Some(inner));
        })
        .await;

        // The inner scope is over: the outer one is back.
        assert_eq!(current_user_id(), Some(outer));
    })
    .await;
}

#[tokio::test]
async fn a_spawned_task_does_not_inherit_the_actor() {
    let actor = Uuid::new_v4();

    let seen = scope(Some(actor), async {
        tokio::spawn(async { current_user_id() })
            .await
            .expect("the spawned task should not panic")
    })
    .await;

    assert_eq!(seen, None, "task-locals are not inherited by spawned tasks");
}

#[tokio::test]
async fn a_spawned_task_can_scope_its_own_actor() {
    let actor = Uuid::new_v4();
    let own = Uuid::new_v4();

    let seen = scope(Some(actor), async {
        tokio::spawn(async move { scope(Some(own), async { current_user_id() }).await })
            .await
            .expect("the spawned task should not panic")
    })
    .await;

    assert_eq!(seen, Some(own));
}

#[tokio::test]
async fn the_actor_reaches_the_audit_row() {
    let id = Uuid::new_v4();
    let actor = Uuid::new_v4();

    let db = MockDatabase::new(DbBackend::Postgres)
        .append_query_results([[user(id)]])
        .append_exec_results([MockExecResult {
            rows_affected: 1,
            ..Default::default()
        }])
        .into_connection();

    scope(Some(actor), async {
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
    })
    .await;

    let log = db.into_transaction_log();
    let statement = log.last().unwrap().statements().last().unwrap();
    let bindings = insert_bindings(statement);

    assert_eq!(bindings["updated_by_id"], Value::Uuid(Some(actor)));
}

#[tokio::test]
async fn a_write_in_a_spawned_task_without_a_scope_fails() {
    let id = Uuid::new_v4();
    let db = MockDatabase::new(DbBackend::Postgres)
        .append_query_results([[user(id)]])
        .into_connection();

    let result = scope(Some(Uuid::new_v4()), async {
        tokio::spawn(async move {
            ActiveModel {
                id: Set(id),
                name: Set("Ada".to_owned()),
                email: Set(None),
                password_hash: Set(None),
                age: Set(36),
            }
            .insert(&db)
            .await
        })
        .await
        .expect("the spawned task should not panic")
    })
    .await;

    let Err(DbErr::Custom(message)) = result else {
        panic!("expected the write to fail without a scope");
    };
    assert!(
        message.contains("no audit scope"),
        "expected a missing-scope error, got: {message}"
    );
}
