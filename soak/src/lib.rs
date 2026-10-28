//! Shared pieces of the soak test: the audited entity, the schema,
//! and the operation-log contract between the writers and the
//! verifier. The design lives in this directory's README.md.

use serde::{Deserialize, Serialize};

pub mod notes {
    use sea_orm::entity::prelude::*;
    use serde::{Deserialize, Serialize};

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "notes")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        #[sea_orm(column_type = "Text")]
        pub list_name: String,
        #[sea_orm(column_type = "Text")]
        pub title: String,
        #[sea_orm(column_type = "Text", nullable)]
        pub body: Option<String>,
        /// The redacted column. Writers plant unique sentinel values
        /// here; a sentinel in the audits table is a violation.
        #[sea_orm(column_type = "Text", nullable)]
        pub secret_token: Option<String>,
        /// The excluded column. Same sentinel treatment as above.
        #[sea_orm(column_type = "Text", nullable)]
        pub internal_note: Option<String>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    seaorm_auto_audit::impl_audit_hooks!(Entity, ActiveModel);

    impl seaorm_auto_audit::Auditable for Entity {
        fn redacted_columns() -> &'static [&'static str] {
            &["secret_token"]
        }

        fn excluded_columns() -> &'static [&'static str] {
            &["internal_note"]
        }
    }
}

/// Two column types split by backend. The key: Postgres wants a
/// native uuid, MySQL a BINARY(16), the same split the crate's own
/// real-database suites use. And list_name: VARCHAR(191) on MySQL,
/// text on Postgres, because the bulk wrappers filter on it and it
/// is indexed on both backends, and a MySQL index over a TEXT
/// column needs a prefix length. The index itself matters as much
/// as its type: an unindexed filter column makes concurrent bulk
/// operations take gap locks on MySQL until they deadlock (error
/// 1213, seen in the first MySQL shakedown). A consumer's filtered
/// column would be indexed; the soak schema matches.
const NOTES_DDL_POSTGRES: &str = r#"
CREATE TABLE IF NOT EXISTS notes (
    id uuid PRIMARY KEY,
    list_name text NOT NULL,
    title text NOT NULL,
    body text,
    secret_token text,
    internal_note text
);
CREATE INDEX IF NOT EXISTS notes_list_name_idx ON notes (list_name);
"#;

const NOTES_DDL_MYSQL: &str = r#"
CREATE TABLE IF NOT EXISTS notes (
    id BINARY(16) PRIMARY KEY,
    list_name VARCHAR(191) NOT NULL,
    title TEXT NOT NULL,
    body TEXT,
    secret_token TEXT,
    internal_note TEXT,
    INDEX notes_list_name_idx (list_name)
);
"#;

/// The notes schema for the backend a database URL names.
pub fn notes_ddl(database_url: &str) -> &'static str {
    if database_url.starts_with("mysql") {
        NOTES_DDL_MYSQL
    } else {
        NOTES_DDL_POSTGRES
    }
}

const AUDITS_DDL_POSTGRES: &str = include_str!("../../sql/setup.postgres.sql");
const AUDITS_DDL_MYSQL: &str = include_str!("../../sql/setup.mysql.sql");

/// The crate's own setup file for the backend a database URL names.
pub fn audits_ddl(database_url: &str) -> &'static str {
    if database_url.starts_with("mysql") {
        AUDITS_DDL_MYSQL
    } else {
        AUDITS_DDL_POSTGRES
    }
}

/// Apply a DDL script statement by statement. The crate's setup
/// files hold several statements, and the MySQL driver refuses a
/// multi-statement string (Postgres tolerates one, which is why
/// this only bites on MySQL). Mirrors the `apply` helper in the
/// crate's own real-database suites, comment lines and all.
pub async fn apply_ddl<C: sea_orm::ConnectionTrait>(
    db: &C,
    script: &str,
) -> Result<(), sea_orm::DbErr> {
    let script = script
        .lines()
        .filter(|line| !line.trim_start().starts_with("--"))
        .collect::<Vec<_>>()
        .join("\n");
    for statement in script.split(';') {
        let statement = statement.trim();
        if statement.is_empty() {
            continue;
        }
        db.execute_unprepared(statement).await?;
    }
    Ok(())
}

/// One note as the writer sends it. The sentinel fields are accepted
/// on writes and never returned by any endpoint.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NoteInput {
    pub title: String,
    pub body: Option<String>,
    pub secret_token: Option<String>,
    pub internal_note: Option<String>,
}

/// One entry in a writer's operation log (one JSON object per line).
///
/// The log is the verifier's ground truth about intent: which writer
/// (and therefore which actor) did what to which row, with what
/// values, and how the operation ended. Writers make titles unique
/// per operation, so the verifier can correlate exactly instead of
/// guessing which audit row belongs to which intent.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OpEntry {
    /// The writer's own sequence number, from 0.
    pub seq: u64,
    /// The writer's name (writer-0, writer-1, ...).
    pub writer: String,
    /// The actor the writer sends as its `x-actor-id`.
    pub actor: String,
    /// create, update, delete, bulk_create, bulk_update, bulk_delete,
    /// read, transact_fail, or import.
    pub op: String,
    /// The rows the operation targeted or produced.
    pub row_ids: Vec<String>,
    /// The list a bulk operation worked on, when it worked on one.
    pub list_name: Option<String>,
    /// committed, rolled_back, suspended, or missing (the row was
    /// already gone; the API answered 404 and nothing was audited).
    pub outcome: String,
    /// The public values the operation set (title, body), for the
    /// truthfulness checks. Never any sentinel value.
    pub values: Option<serde_json::Value>,
    /// The sentinel secrets this operation planted in the redacted
    /// and excluded columns. The verifier asserts that none of them
    /// appears anywhere in the audits table.
    pub sentinels: Vec<String>,
}
