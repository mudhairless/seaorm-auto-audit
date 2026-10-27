/// Generate the real-database suite for one audited entity.
///
/// Every invocation lives in its own module so the test names do not collide.
/// The entity may use any primary-key type supported by [`crate::AuditId`];
/// the only requirement is that it has the same columns as `common::Model`.
#[allow(unused_macros)]
macro_rules! real_database_suite {
    ($module:ident, $entity:path, $key:ty) => {
        mod $module {
            use super::*;
            use sea_orm::{
                ActiveModelTrait,
                ActiveValue::{NotSet, Set, Unchanged},
                ColumnTrait, Condition, EntityName, EntityTrait, TransactionTrait,
            };
            use seaorm_auto_audit::{
                RequestMetadata, delete_by_id as audited_delete_by_id,
                delete_many as audited_delete_many, insert_many as audited_insert_many, scope,
                scope_with_request, update_many as audited_update_many,
            };
            use serde_json::json;

            use $entity as Entity;

            use $crate::common::KeyFixture;

            type ActiveModel = <Entity as sea_orm::EntityTrait>::ActiveModel;
            type Column = <Entity as sea_orm::EntityTrait>::Column;
            type Key = $key;

            fn new_key() -> Key {
                <Key as KeyFixture>::new_key()
            }

            #[tokio::test]
            async fn a_write_is_audited_in_a_real_database() {
                let Some(db) = database().await else {
                    return;
                };
                let id = new_key();
                let actor = new_key();

                let txn = db.begin().await.expect("the transaction should start");
                scope(Some(actor.clone().into()), async {
                    ActiveModel {
                        id: Set(id.clone()),
                        name: Set("Ada".to_owned()),
                        email: Set(Some("ada@example.com".to_owned())),
                        password_hash: Set(Some("hash-before".to_owned())),
                        age: Set(36),
                    }
                    .insert(&txn)
                    .await
                    .expect("the insert should succeed");
                })
                .await;
                txn.commit().await.expect("the transaction should commit");

                let audits = audits_of(&db, &id).await;
                assert_eq!(audits.len(), 1, "one audit row per write");

                let audit = &audits[0];
                assert_eq!(audit.auditable_table, Entity::default().table_name());
                assert_eq!(audit.auditable_id, id.to_string());
                assert_eq!(audit.action, "create");
                assert_eq!(audit.updated_by_id, Some(actor.to_string()));
                assert!(audit.before_content.is_none(), "a create has no before");

                let after = audit.after_content.as_ref().expect("an after");
                assert_eq!(after["name"], json!("Ada"));
                assert!(
                    after.get("password_hash").is_none(),
                    "the credential is never stored: {after}"
                );
            }

            #[tokio::test]
            async fn an_update_records_the_before_state() {
                let Some(db) = database().await else {
                    return;
                };
                let id = new_key();
                let actor = new_key();

                let txn = db.begin().await.expect("the transaction should start");
                scope(Some(actor.into()), async {
                    ActiveModel {
                        id: Set(id.clone()),
                        name: Set("Ada".to_owned()),
                        email: Set(None),
                        password_hash: Set(Some("hash-before".to_owned())),
                        age: Set(36),
                    }
                    .insert(&txn)
                    .await
                    .expect("the insert should succeed");

                    ActiveModel {
                        id: Unchanged(id.clone()),
                        name: Set("Grace".to_owned()),
                        email: NotSet,
                        password_hash: NotSet,
                        age: Set(37),
                    }
                    .update(&txn)
                    .await
                    .expect("the update should succeed");
                })
                .await;
                txn.commit().await.expect("the transaction should commit");

                let audits = audits_of(&db, &id).await;
                assert_eq!(audits.len(), 2, "one audit row per write");

                let update = audits
                    .iter()
                    .find(|audit| audit.action == "update")
                    .expect("an audit row for the update");

                let before = update.before_content.as_ref().expect("a before");
                let after = update.after_content.as_ref().expect("an after");
                assert_eq!(before["name"], json!("Ada"));
                assert_eq!(after["name"], json!("Grace"));
                assert_eq!(before["age"], json!(36));
                assert_eq!(after["age"], json!(37));
            }

            #[tokio::test]
            async fn a_credential_change_is_marked_but_never_stored() {
                let Some(db) = database().await else {
                    return;
                };
                let id = new_key();
                let actor = new_key();

                let txn = db.begin().await.expect("the transaction should start");
                scope(Some(actor.into()), async {
                    ActiveModel {
                        id: Set(id.clone()),
                        name: Set("Ada".to_owned()),
                        email: Set(None),
                        password_hash: Set(Some("hash-before".to_owned())),
                        age: Set(36),
                    }
                    .insert(&txn)
                    .await
                    .expect("the insert should succeed");

                    ActiveModel {
                        id: Unchanged(id.clone()),
                        name: NotSet,
                        email: NotSet,
                        password_hash: Set(Some("hash-after".to_owned())),
                        age: NotSet,
                    }
                    .update(&txn)
                    .await
                    .expect("the update should succeed");
                })
                .await;
                txn.commit().await.expect("the transaction should commit");

                let audits = audits_of(&db, &id).await;
                let update = audits
                    .iter()
                    .find(|audit| audit.action == "update")
                    .expect("an audit row for the update");

                let before = update.before_content.as_ref().expect("a before");
                let after = update.after_content.as_ref().expect("an after");
                assert_eq!(before["password_hash"], json!(seaorm_auto_audit::REDACTED));
                assert_eq!(after["password_hash"], json!(seaorm_auto_audit::REDACTED));

                let row = format!("{before}{after}");
                assert!(!row.contains("hash-before"), "the old hash leaked: {row}");
                assert!(!row.contains("hash-after"), "the new hash leaked: {row}");
            }

            #[tokio::test]
            async fn a_rolled_back_write_leaves_no_audit_row() {
                let Some(db) = database().await else {
                    return;
                };
                let id = new_key();
                let actor = new_key();

                let txn = db.begin().await.expect("the transaction should start");
                scope(Some(actor.into()), async {
                    ActiveModel {
                        id: Set(id.clone()),
                        name: Set("Ada".to_owned()),
                        email: Set(None),
                        password_hash: Set(None),
                        age: Set(36),
                    }
                    .insert(&txn)
                    .await
                    .expect("the insert should succeed");
                })
                .await;
                txn.rollback()
                    .await
                    .expect("the transaction should roll back");

                // The write and its audit row rolled back together.
                let row = Entity::find_by_id(id.clone())
                    .one(&db)
                    .await
                    .expect("the row should be readable");
                assert!(row.is_none(), "the row was rolled back");

                let audits = audits_of(&db, &id).await;
                assert!(audits.is_empty(), "the audit row was rolled back too");
            }

            #[tokio::test]
            async fn a_machine_made_change_is_audited_without_an_actor() {
                let Some(db) = database().await else {
                    return;
                };
                let id = new_key();

                let txn = db.begin().await.expect("the transaction should start");
                scope(None, async {
                    ActiveModel {
                        id: Set(id.clone()),
                        name: Set("Ada".to_owned()),
                        email: Set(None),
                        password_hash: Set(None),
                        age: Set(36),
                    }
                    .insert(&txn)
                    .await
                    .expect("the insert should succeed");
                })
                .await;
                txn.commit().await.expect("the transaction should commit");

                let audits = audits_of(&db, &id).await;
                assert_eq!(audits.len(), 1);
                assert_eq!(audits[0].updated_by_id, None, "nobody was behind it");
                assert_eq!(audits[0].action, "create");
            }

            #[tokio::test]
            async fn a_delete_is_audited() {
                let Some(db) = database().await else {
                    return;
                };
                let id = new_key();
                let actor = new_key();

                let txn = db.begin().await.expect("the transaction should start");
                scope(Some(actor.clone().into()), async {
                    ActiveModel {
                        id: Set(id.clone()),
                        name: Set("Ada".to_owned()),
                        email: Set(None),
                        password_hash: Set(Some("hash-before".to_owned())),
                        age: Set(36),
                    }
                    .insert(&txn)
                    .await
                    .expect("the insert should succeed");

                    ActiveModel {
                        id: Unchanged(id.clone()),
                        name: NotSet,
                        email: NotSet,
                        password_hash: NotSet,
                        age: NotSet,
                    }
                    .delete(&txn)
                    .await
                    .expect("the delete should succeed");
                })
                .await;
                txn.commit().await.expect("the transaction should commit");

                let audits = audits_of(&db, &id).await;
                let delete = audits
                    .iter()
                    .find(|audit| audit.action == "delete")
                    .expect("an audit row for the delete");

                assert_eq!(delete.updated_by_id, Some(actor.to_string()));
                assert!(delete.after_content.is_none(), "a delete has no after");

                let before = delete.before_content.as_ref().expect("a before");
                assert_eq!(before["name"], json!("Ada"));
                assert!(before.get("password_hash").is_none());
            }

            /// Two rows of the audited table, in the shape `insert_many` takes.
            fn rows(ids: [Key; 2]) -> Vec<ActiveModel> {
                ids.into_iter()
                    .map(|id| ActiveModel {
                        id: Set(id),
                        name: Set("Ada".to_owned()),
                        email: Set(None),
                        password_hash: Set(Some("hash-before".to_owned())),
                        age: Set(36),
                    })
                    .collect()
            }

            /// The request details a middleware would have captured.
            fn request_metadata() -> RequestMetadata {
                RequestMetadata {
                    ip_address: Some("203.0.113.7".to_owned()),
                    url: Some("http://api.example.com/users".to_owned()),
                    user_agent: Some("ftg-test/1.0".to_owned()),
                }
            }

            #[tokio::test]
            async fn insert_many_audits_every_row_and_records_the_request() {
                let Some(db) = database().await else {
                    return;
                };
                let ids = [new_key(), new_key()];
                let actor = new_key();

                let txn = db.begin().await.expect("the transaction should start");
                scope_with_request(Some(actor.clone().into()), Some(request_metadata()), async {
                    audited_insert_many::<Entity, _, _>(&txn, rows(ids.clone()))
                        .await
                        .expect("the bulk insert should succeed");
                })
                .await;
                txn.commit().await.expect("the transaction should commit");

                for id in ids {
                    let audits = audits_of(&db, &id).await;
                    assert_eq!(audits.len(), 1, "one audit row per inserted row");

                    let audit = &audits[0];
                    assert_eq!(audit.action, "create");
                    assert_eq!(audit.updated_by_id, Some(actor.to_string()));
                    assert_eq!(audit.ip_address.as_deref(), Some("203.0.113.7"));
                    assert_eq!(audit.url.as_deref(), Some("http://api.example.com/users"));
                    assert_eq!(audit.user_agent.as_deref(), Some("ftg-test/1.0"));

                    let after = audit.after_content.as_ref().expect("an after");
                    assert_eq!(after["name"], json!("Ada"));
                    assert!(
                        after.get("password_hash").is_none(),
                        "the credential is never stored: {after}"
                    );
                }
            }

            #[tokio::test]
            async fn update_many_audits_every_row_it_changed() {
                let Some(db) = database().await else {
                    return;
                };
                let ids = [new_key(), new_key()];
                let actor = new_key();

                let txn = db.begin().await.expect("the transaction should start");
                scope(Some(actor.into()), async {
                    audited_insert_many::<Entity, _, _>(&txn, rows(ids.clone()))
                        .await
                        .expect("the seed insert should succeed");

                    let filter = Condition::all().add(Column::Id.is_in(ids.clone()));
                    let values = ActiveModel {
                        id: NotSet,
                        name: Set("Grace".to_owned()),
                        email: NotSet,
                        password_hash: Set(Some("hash-after".to_owned())),
                        age: Set(37),
                    };
                    let result = audited_update_many::<Entity, _>(&txn, filter, values)
                        .await
                        .expect("the bulk update should succeed");
                    assert_eq!(result.rows_affected, 2);
                })
                .await;
                txn.commit().await.expect("the transaction should commit");

                for id in ids {
                    let audits = audits_of(&db, &id).await;
                    let update = audits
                        .iter()
                        .find(|audit| audit.action == "update")
                        .expect("an audit row for the update");

                    let before = update.before_content.as_ref().expect("a before");
                    let after = update.after_content.as_ref().expect("an after");
                    assert_eq!(before["name"], json!("Ada"));
                    assert_eq!(after["name"], json!("Grace"));
                    assert_eq!(before["age"], json!(36));
                    assert_eq!(after["age"], json!(37));

                    // The same reconcile as the hook path: a changed credential is marked
                    // on both sides, and neither hash is stored.
                    assert_eq!(before["password_hash"], json!(seaorm_auto_audit::REDACTED));
                    assert_eq!(after["password_hash"], json!(seaorm_auto_audit::REDACTED));
                }
            }

            #[tokio::test]
            async fn delete_many_audits_every_row_it_removed() {
                let Some(db) = database().await else {
                    return;
                };
                let ids = [new_key(), new_key()];
                let actor = new_key();

                let txn = db.begin().await.expect("the transaction should start");
                scope(Some(actor.into()), async {
                    audited_insert_many::<Entity, _, _>(&txn, rows(ids.clone()))
                        .await
                        .expect("the seed insert should succeed");

                    let filter = Condition::all().add(Column::Id.is_in(ids.clone()));
                    let result = audited_delete_many::<Entity, _>(&txn, filter)
                        .await
                        .expect("the bulk delete should succeed");
                    assert_eq!(result.rows_affected, 2);
                })
                .await;
                txn.commit().await.expect("the transaction should commit");

                for id in ids {
                    let audits = audits_of(&db, &id).await;
                    let delete = audits
                        .iter()
                        .find(|audit| audit.action == "delete")
                        .expect("an audit row for the delete");

                    assert!(delete.after_content.is_none(), "a delete has no after");
                    let before = delete.before_content.as_ref().expect("a before");
                    assert_eq!(before["name"], json!("Ada"));
                    assert!(
                        before.get("password_hash").is_none(),
                        "the credential is never stored: {before}"
                    );
                }
            }

            #[tokio::test]
            async fn an_update_that_changes_nothing_writes_no_audit_row() {
                let Some(db) = database().await else {
                    return;
                };
                let id = new_key();
                let actor = new_key();

                let txn = db.begin().await.expect("the transaction should start");
                scope(Some(actor.into()), async {
                    ActiveModel {
                        id: Set(id.clone()),
                        name: Set("Ada".to_owned()),
                        email: Set(Some("ada@example.com".to_owned())),
                        password_hash: Set(Some("hash-before".to_owned())),
                        age: Set(36),
                    }
                    .insert(&txn)
                    .await
                    .expect("the insert should succeed");

                    // Every column is set to the value it already has.
                    ActiveModel {
                        id: Unchanged(id.clone()),
                        name: Set("Ada".to_owned()),
                        email: Set(Some("ada@example.com".to_owned())),
                        password_hash: Set(Some("hash-before".to_owned())),
                        age: Set(36),
                    }
                    .update(&txn)
                    .await
                    .expect("the update should succeed");
                })
                .await;
                txn.commit().await.expect("the transaction should commit");

                let audits = audits_of(&db, &id).await;
                assert_eq!(audits.len(), 1, "only the create is audited");
                assert_eq!(audits[0].action, "create");
            }

            #[tokio::test]
            async fn update_many_skips_rows_it_left_unchanged() {
                let Some(db) = database().await else {
                    return;
                };
                let changed_id = new_key();
                let unchanged_id = new_key();
                let actor = new_key();

                let row = |id: Key, name: &str| ActiveModel {
                    id: Set(id),
                    name: Set(name.to_owned()),
                    email: Set(None),
                    password_hash: Set(Some("hash-before".to_owned())),
                    age: Set(36),
                };

                let txn = db.begin().await.expect("the transaction should start");
                scope(Some(actor.into()), async {
                    audited_insert_many::<Entity, _, _>(
                        &txn,
                        [
                            row(changed_id.clone(), "Ada"),
                            row(unchanged_id.clone(), "Grace"),
                        ],
                    )
                    .await
                    .expect("the seed insert should succeed");

                    // One row moves to "Grace"; the other is already there.
                    let filter = Condition::all()
                        .add(Column::Id.is_in([changed_id.clone(), unchanged_id.clone()]));
                    let values = ActiveModel {
                        id: NotSet,
                        name: Set("Grace".to_owned()),
                        email: NotSet,
                        password_hash: NotSet,
                        age: NotSet,
                    };
                    let result = audited_update_many::<Entity, _>(&txn, filter, values)
                        .await
                        .expect("the bulk update should succeed");
                    assert_eq!(result.rows_affected, 2);
                })
                .await;
                txn.commit().await.expect("the transaction should commit");

                let changed_audits = audits_of(&db, &changed_id).await;
                let updates = changed_audits
                    .iter()
                    .filter(|audit| audit.action == "update")
                    .count();
                assert_eq!(updates, 1, "the changed row is audited");

                let unchanged_audits = audits_of(&db, &unchanged_id).await;
                let updates = unchanged_audits
                    .iter()
                    .filter(|audit| audit.action == "update")
                    .count();
                assert_eq!(updates, 0, "the unchanged row is not audited");
            }

            #[tokio::test]
            async fn update_many_matching_nothing_writes_no_audit_rows() {
                let Some(db) = database().await else {
                    return;
                };
                let missing = new_key();
                let actor = new_key();

                let txn = db.begin().await.expect("the transaction should start");
                let result = scope(Some(actor.into()), async {
                                        // Seed a row first: on SQLite a transaction that reads
                    // before it writes cannot always upgrade its lock while
                    // other suites are writing, so the write comes first, the
                    // same shape the matching-row tests use.
                    ActiveModel {
                        id: Set(new_key()),
                        name: Set("Ada".to_owned()),
                        email: Set(None),
                        password_hash: Set(None),
                        age: Set(36),
                    }
                    .insert(&txn)
                    .await
                    .expect("the seed insert should succeed");

let filter = Condition::all().add(Column::Id.eq(missing.clone()));
                    let values = ActiveModel {
                        id: NotSet,
                        name: Set("Grace".to_owned()),
                        email: NotSet,
                        password_hash: NotSet,
                        age: NotSet,
                    };
                    audited_update_many::<Entity, _>(&txn, filter, values)
                        .await
                        .expect("an update matching nothing should succeed")
                })
                .await;
                txn.commit().await.expect("the transaction should commit");

                assert_eq!(result.rows_affected, 0);
                assert!(
                    audits_of(&db, &missing).await.is_empty(),
                    "no audit rows for an update that matched nothing"
                );
            }

            #[tokio::test]
            async fn delete_many_matching_nothing_writes_no_audit_rows() {
                let Some(db) = database().await else {
                    return;
                };
                let missing = new_key();
                let actor = new_key();

                let txn = db.begin().await.expect("the transaction should start");
                let result = scope(Some(actor.into()), async {
                    let filter = Condition::all().add(Column::Id.eq(missing.clone()));
                                        // Seed a row first, for the SQLite lock reason the
                    // matching-nothing update test describes.
                    ActiveModel {
                        id: Set(new_key()),
                        name: Set("Ada".to_owned()),
                        email: Set(None),
                        password_hash: Set(None),
                        age: Set(36),
                    }
                    .insert(&txn)
                    .await
                    .expect("the seed insert should succeed");

audited_delete_many::<Entity, _>(&txn, filter)
                        .await
                        .expect("a delete matching nothing should succeed")
                })
                .await;
                txn.commit().await.expect("the transaction should commit");

                assert_eq!(result.rows_affected, 0);
                assert!(
                    audits_of(&db, &missing).await.is_empty(),
                    "no audit rows for a delete that matched nothing"
                );
            }

            #[tokio::test]
            async fn delete_by_id_is_audited() {
                let Some(db) = database().await else {
                    return;
                };
                let id = new_key();
                let actor = new_key();

                let txn = db.begin().await.expect("the transaction should start");
                scope(Some(actor.clone().into()), async {
                    ActiveModel {
                        id: Set(id.clone()),
                        name: Set("Ada".to_owned()),
                        email: Set(None),
                        password_hash: Set(None),
                        age: Set(36),
                    }
                    .insert(&txn)
                    .await
                    .expect("the seed insert should succeed");

                    let result = audited_delete_by_id::<Entity, _, _>(&txn, id.clone())
                        .await
                        .expect("the delete should succeed");
                    assert_eq!(result.rows_affected, 1);
                })
                .await;
                txn.commit().await.expect("the transaction should commit");

                let audits = audits_of(&db, &id).await;
                let delete = audits
                    .iter()
                    .find(|audit| audit.action == "delete")
                    .expect("an audit row for the delete");

                assert_eq!(delete.updated_by_id, Some(actor.to_string()));
                assert!(delete.after_content.is_none(), "a delete has no after");
                let before = delete.before_content.as_ref().expect("a before");
                assert_eq!(before["name"], json!("Ada"));
            }
        }
    };
}

#[allow(unused_imports)]
pub(crate) use real_database_suite;
