//! Pruning the audit trail: an explicit operation the caller runs.
//!
//! The crate never prunes on write and never schedules anything. An audit
//! trail exists to be kept, so removing rows from it is a decision the
//! application makes on purpose, in its own maintenance job or admin tool,
//! by calling [`prune`].
//!
//! A prune always reports: both modes return the rows in question, oldest
//! first. [`PruneMode::DryRun`] returns the rows the policy selects and
//! deletes nothing, so the first run of a new policy is inspectable before
//! it is trusted. [`PruneMode::Delete`] removes the rows inside a
//! transaction and returns the rows it removed, so pruning and backing up
//! are one step: the caller can write the returned rows wherever removed
//! history is kept.

use std::collections::HashSet;
use std::time::Duration;

use sea_orm::{
    ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter, QueryOrder, TransactionSession,
    TransactionTrait,
};
use uuid::Uuid;

use crate::audits;

/// How much of the audit trail to keep.
///
/// The two rules combine as a union: a row is pruned when it is older than
/// `older_than`, or when it falls beyond the `keep_latest` most recent rows
/// for its (table, id) pair, or both. At least one rule must be set; a
/// policy with neither is refused rather than treated as licence to prune
/// by no rule at all.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrunePolicy {
    /// Keep only this many of the most recent audits per (table, id).
    /// `Some(0)` keeps none of them.
    pub keep_latest: Option<u64>,
    /// Remove audits older than this age, measured from now.
    pub older_than: Option<Duration>,
}

impl PrunePolicy {
    /// A policy that keeps the `keep` most recent audits per (table, id).
    pub fn keep_latest(keep: u64) -> Self {
        Self {
            keep_latest: Some(keep),
            older_than: None,
        }
    }

    /// A policy that removes audits older than `age`.
    pub fn older_than(age: Duration) -> Self {
        Self {
            keep_latest: None,
            older_than: Some(age),
        }
    }
}

/// Whether a [`prune`] run only reports, or reports and deletes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum PruneMode {
    /// Return the rows the policy selects; delete nothing.
    DryRun,
    /// Delete the rows the policy selects, inside a transaction, and
    /// return the rows that were removed.
    Delete,
}

/// Prune the `audits` table by `policy`, in the given `mode`.
///
/// Selection is worked out by reading the table: the age rule is one
/// filtered query, and the keep-latest rule walks the whole table ordered
/// by (table, id, newest first). That makes prune an offline maintenance
/// operation rather than something to run on a request path, which is the
/// intent.
///
/// # Errors
///
/// Returns an error when the policy sets neither rule, when `older_than`
/// is too large to represent, or when the database does.
pub async fn prune<C>(
    db: &C,
    policy: &PrunePolicy,
    mode: PruneMode,
) -> Result<Vec<audits::Model>, DbErr>
where
    C: ConnectionTrait + TransactionTrait,
{
    if policy.keep_latest.is_none() && policy.older_than.is_none() {
        return Err(DbErr::Custom(
            "seaorm-auto-audit: a prune policy must set `keep_latest` or `older_than` — \
             refusing to prune by no rule at all"
                .to_owned(),
        ));
    }

    let mut doomed: Vec<audits::Model> = Vec::new();
    let mut seen: HashSet<Uuid> = HashSet::new();

    if let Some(age) = policy.older_than {
        let age = chrono::Duration::from_std(age).map_err(|_| {
            DbErr::Custom("seaorm-auto-audit: `older_than` is too large to represent".to_owned())
        })?;
        let cutoff = (chrono::Utc::now() - age).fixed_offset();
        let rows = audits::Entity::find()
            .filter(audits::Column::CreatedAt.lt(cutoff))
            .all(db)
            .await?;
        for row in rows {
            if seen.insert(row.id) {
                doomed.push(row);
            }
        }
    }

    if let Some(keep) = policy.keep_latest {
        let rows = audits::Entity::find()
            .order_by_asc(audits::Column::AuditableTable)
            .order_by_asc(audits::Column::AuditableId)
            .order_by_desc(audits::Column::CreatedAt)
            .order_by_desc(audits::Column::Id)
            .all(db)
            .await?;
        let mut group: Option<(&str, &str)> = None;
        let mut kept: u64 = 0;
        for row in &rows {
            let key = (row.auditable_table.as_str(), row.auditable_id.as_str());
            if group != Some(key) {
                group = Some(key);
                kept = 0;
            }
            kept += 1;
            if kept > keep && seen.insert(row.id) {
                doomed.push(row.clone());
            }
        }
    }

    doomed.sort_by(|a, b| {
        a.created_at
            .cmp(&b.created_at)
            .then_with(|| a.id.cmp(&b.id))
    });

    match mode {
        PruneMode::DryRun => Ok(doomed),
        PruneMode::Delete => {
            if doomed.is_empty() {
                return Ok(doomed);
            }
            let ids: Vec<Uuid> = doomed.iter().map(|row| row.id).collect();
            let txn = db.begin().await?;
            // Re-read the selected rows inside the transaction, so the rows
            // returned are exactly the rows the delete removes, even if
            // another prune ran in between.
            //
            // Both the re-read and the delete below are sliced by
            // STATEMENT_CHUNK ids. One un-sliced `is_in` list puts one
            // variable per id in a single statement, and SQLite allows at
            // most 32,766 variables, so a prune selecting more than about
            // 32k rows failed the way bulk writes used to. The ids arrive
            // in oldest-first order and each slice keeps that order, so
            // concatenating the slices keeps the returned rows ordered.
            let mut removed = Vec::new();
            for ids in ids.chunks(crate::STATEMENT_CHUNK) {
                let mut chunk = audits::Entity::find()
                    .filter(audits::Column::Id.is_in(ids.to_vec()))
                    .order_by_asc(audits::Column::CreatedAt)
                    .order_by_asc(audits::Column::Id)
                    .all(&txn)
                    .await?;
                removed.append(&mut chunk);
            }
            let removed_ids: Vec<Uuid> = removed.iter().map(|row| row.id).collect();
            for ids in removed_ids.chunks(crate::STATEMENT_CHUNK) {
                audits::Entity::delete_many()
                    .filter(audits::Column::Id.is_in(ids.to_vec()))
                    .exec(&txn)
                    .await?;
            }
            txn.commit().await?;
            Ok(removed)
        }
    }
}
