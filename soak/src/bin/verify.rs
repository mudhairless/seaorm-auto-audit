//! The soak verifier: reconcile the writers' intent logs against
//! the audit trail and the final state of the notes table, and
//! check the five invariants from this directory's README.
//!
//! Exit codes mirror the crate's checker binary: 0 the run is
//! clean, 1 invariant violations were found, 2 the verification
//! itself could not run (no logs, database unreachable).
//!
//! Configuration: DATABASE_URL, LOG_DIR (default "logs").

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::process::ExitCode;

use sea_orm::{Database, EntityTrait, QueryOrder};
use serde_json::Value as JsonValue;
use uuid::Uuid;

use seaorm_auto_audit::audits;
use seaorm_auto_audit_soak::notes;
use seaorm_auto_audit_soak::OpEntry;

struct AuditRow {
    pos: usize,
    auditable_id: String,
    action: String,
    actor: Option<String>,
    before: Option<JsonValue>,
    after: Option<JsonValue>,
    comment: Option<String>,
    claimed_by: Option<String>,
}

fn content_text(value: &Option<JsonValue>) -> String {
    value.as_ref().map(ToString::to_string).unwrap_or_default()
}

fn content_title(value: &Option<JsonValue>) -> Option<&str> {
    value
        .as_ref()
        .and_then(|v| v.get("title"))
        .and_then(|t| t.as_str())
}

fn content_body(value: &Option<JsonValue>) -> Option<&str> {
    value
        .as_ref()
        .and_then(|v| v.get("body"))
        .and_then(|t| t.as_str())
}

/// Find an unclaimed audit row matching action, row id and a
/// content predicate; claim it for `who` and return its actor.
///
/// A row carrying the expected actor wins first: reads of one row
/// by several writers leave indistinguishable rows behind, and
/// pairing them in arrival order would report crossed actors that
/// never happened. The fallback to any matching row keeps a
/// genuinely wrong actor claimable, so the caller still flags it.
fn claim(
    rows: &mut [AuditRow],
    action: &str,
    row_id: &str,
    who: &str,
    expect_actor: &str,
    pred: impl Fn(&AuditRow) -> bool,
) -> Option<Option<String>> {
    let find = |want_actor: bool| {
        rows.iter().position(|row| {
            row.claimed_by.is_none()
                && row.action == action
                && row.auditable_id == row_id
                && pred(row)
                && (!want_actor || row.actor.as_deref() == Some(expect_actor))
        })
    };
    let found = find(true).or_else(|| find(false));
    found.map(|idx| {
        rows[idx].claimed_by = Some(who.to_owned());
        rows[idx].actor.clone()
    })
}

fn entry_key(entry: &OpEntry) -> String {
    format!("{}:{}", entry.writer, entry.seq)
}

async fn run() -> Result<Vec<String>, String> {
    let url = std::env::var("DATABASE_URL")
        .map_err(|_| "DATABASE_URL names the soak database".to_owned())?;
    let log_dir = std::env::var("LOG_DIR").unwrap_or_else(|_| "logs".to_owned());

    // The writers' intent: every .jsonl file in the log directory.
    let mut entries: Vec<OpEntry> = Vec::new();
    let mut log_files: Vec<_> = std::fs::read_dir(&log_dir)
        .map_err(|e| format!("cannot read log directory {log_dir}: {e}"))?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "jsonl"))
        .collect();
    log_files.sort();
    for path in &log_files {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        for (line_no, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let entry: OpEntry = serde_json::from_str(line).map_err(|e| {
                format!(
                    "{} line {} is not an OpEntry: {e}",
                    path.display(),
                    line_no + 1
                )
            })?;
            entries.push(entry);
        }
    }
    if entries.is_empty() {
        return Err(format!("no writer log entries under {log_dir}"));
    }
    let hot_rows: BTreeSet<String> = std::fs::read_to_string(format!("{log_dir}/hot-rows.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<Vec<Uuid>>(&text).ok())
        .map(|ids| ids.iter().map(ToString::to_string).collect())
        .unwrap_or_default();

    let db = Database::connect(&url)
        .await
        .map_err(|e| format!("cannot connect to the soak database: {e}"))?;
    let audit_models = seaorm_auto_audit::Audits::find()
        .order_by_asc(audits::Column::CreatedAt)
        .order_by_asc(audits::Column::Id)
        .all(&db)
        .await
        .map_err(|e| format!("cannot read the audits table: {e}"))?;
    let note_models = notes::Entity::find()
        .all(&db)
        .await
        .map_err(|e| format!("cannot read the notes table: {e}"))?;

    let mut rows: Vec<AuditRow> = audit_models
        .iter()
        .enumerate()
        .map(|(pos, m)| AuditRow {
            pos,
            auditable_id: m.auditable_id.clone(),
            action: m.action.clone(),
            actor: m.updated_by_id.clone(),
            before: m.before_content.clone(),
            after: m.after_content.clone(),
            comment: m.comment.clone(),
            claimed_by: None,
        })
        .collect();
    let mut violations: Vec<String> = Vec::new();
    let notes_by_id: HashMap<String, &notes::Model> =
        note_models.iter().map(|m| (m.id.to_string(), m)).collect();

    // Invariant 4: no sentinel secret anywhere in the trail.
    let sentinels: BTreeSet<&str> = entries
        .iter()
        .flat_map(|e| e.sentinels.iter().map(String::as_str))
        .collect();
    for row in &rows {
        let haystack = format!("{} {}", content_text(&row.before), content_text(&row.after));
        for sentinel in &sentinels {
            if haystack.contains(sentinel) {
                violations.push(format!(
                    "invariant 4: sentinel {sentinel} appears in audit row #{} ({} on {})",
                    row.pos, row.action, row.auditable_id
                ));
            }
        }
    }

    // Invariant 2: nothing from a rolled-back transaction.
    for entry in entries.iter().filter(|e| e.op == "transact_fail") {
        let Some(marker) = entry
            .values
            .as_ref()
            .and_then(|v| v.get("title"))
            .and_then(|t| t.as_str())
        else {
            continue;
        };
        for row in &rows {
            let haystack = format!("{} {}", content_text(&row.before), content_text(&row.after));
            if haystack.contains(marker) {
                violations.push(format!(
                    "invariant 2: rolled-back write {marker} from {} appears in audit row #{}",
                    entry_key(entry),
                    row.pos
                ));
            }
        }
    }

    // Invariant 3: nothing from a suspended scope; the imported
    // rows themselves must exist.
    for entry in entries.iter().filter(|e| e.op == "import") {
        for id in &entry.row_ids {
            if rows.iter().any(|row| &row.auditable_id == id) {
                violations.push(format!(
                    "invariant 3: suspended import {} produced an audit row for {id}",
                    entry_key(entry)
                ));
            }
            if !notes_by_id.contains_key(id) {
                violations.push(format!(
                    "invariant 3: imported row {id} from {} is missing from notes",
                    entry_key(entry)
                ));
            }
        }
    }

    // Invariant 1: every committed entry claims its audit rows,
    // with the writer's actor on them.
    for entry in &entries {
        let who = entry_key(entry);
        match (entry.op.as_str(), entry.outcome.as_str()) {
            ("create", "committed") => {
                let title = entry
                    .values
                    .as_ref()
                    .and_then(|v| v.get("title"))
                    .and_then(|t| t.as_str())
                    .unwrap_or("");
                for id in &entry.row_ids {
                    match claim(&mut rows, "create", id, &who, entry.actor.as_str(), |row| {
                        content_title(&row.after) == Some(title)
                    }) {
                        Some(actor) if actor.as_deref() == Some(entry.actor.as_str()) => {}
                        Some(actor) => violations.push(format!(
                            "invariant 1: create by {who} on {id} carries actor {actor:?}, want {}",
                            entry.actor
                        )),
                        None => violations.push(format!(
                            "invariant 1: committed create {who} on {id} has no audit row"
                        )),
                    }
                }
            }
            ("bulk_create", "committed") => {
                for id in &entry.row_ids {
                    match claim(&mut rows, "create", id, &who, entry.actor.as_str(), |_| true) {
                        Some(actor) if actor.as_deref() == Some(entry.actor.as_str()) => {}
                        Some(actor) => violations.push(format!(
                            "invariant 1: bulk create by {who} on {id} carries actor {actor:?}"
                        )),
                        None => violations.push(format!(
                            "invariant 1: committed bulk_create {who} on {id} has no audit row"
                        )),
                    }
                }
            }
            ("update", "committed") => {
                let title = entry
                    .values
                    .as_ref()
                    .and_then(|v| v.get("title"))
                    .and_then(|t| t.as_str())
                    .unwrap_or("");
                for id in &entry.row_ids {
                    match claim(&mut rows, "update", id, &who, entry.actor.as_str(), |row| {
                        content_title(&row.after) == Some(title)
                    }) {
                        Some(actor) if actor.as_deref() == Some(entry.actor.as_str()) => {}
                        Some(actor) => violations.push(format!(
                            "invariant 1: update by {who} on {id} carries actor {actor:?}"
                        )),
                        None => violations.push(format!(
                            "invariant 1: committed update {who} on {id} (title {title}) has no audit row"
                        )),
                    }
                }
            }
            ("bulk_update", "committed") => {
                let marker = entry
                    .values
                    .as_ref()
                    .and_then(|v| v.get("body"))
                    .and_then(|t| t.as_str())
                    .unwrap_or("");
                let reported = entry
                    .values
                    .as_ref()
                    .and_then(|v| v.get("reported"))
                    .and_then(|t| t.as_u64())
                    .unwrap_or(u64::MAX);
                let mut matched = 0u64;
                for id in &entry.row_ids {
                    if claim(&mut rows, "update", id, &who, entry.actor.as_str(), |row| {
                        content_body(&row.after) == Some(marker)
                    })
                    .is_some()
                    {
                        matched += 1;
                    }
                }
                if matched != entry.row_ids.len() as u64 || matched != reported {
                    violations.push(format!(
                        "invariant 1: bulk_update {who} matched {matched} audit rows for {} logged rows, api reported {reported}",
                        entry.row_ids.len()
                    ));
                }
            }
            ("delete", "committed") => {
                for id in &entry.row_ids {
                    match claim(&mut rows, "delete", id, &who, entry.actor.as_str(), |_| true) {
                        Some(actor) if actor.as_deref() == Some(entry.actor.as_str()) => {}
                        Some(actor) => violations.push(format!(
                            "invariant 1: delete by {who} on {id} carries actor {actor:?}"
                        )),
                        None => violations.push(format!(
                            "invariant 1: committed delete {who} on {id} has no audit row"
                        )),
                    }
                }
            }
            ("bulk_delete", "committed") => {
                let reported = entry
                    .values
                    .as_ref()
                    .and_then(|v| v.get("reported"))
                    .and_then(|t| t.as_u64())
                    .unwrap_or(u64::MAX);
                let mut matched = 0u64;
                for id in &entry.row_ids {
                    if claim(&mut rows, "delete", id, &who, entry.actor.as_str(), |_| true).is_some() {
                        matched += 1;
                    }
                }
                if matched != entry.row_ids.len() as u64 || matched != reported {
                    violations.push(format!(
                        "invariant 1: bulk_delete {who} matched {matched} audit rows for {} logged rows, api reported {reported}",
                        entry.row_ids.len()
                    ));
                }
            }
            ("read", "committed") => {
                for id in &entry.row_ids {
                    match claim(&mut rows, "read", id, &who, entry.actor.as_str(), |row| {
                        row.comment.as_deref() == Some("soak read")
                    }) {
                        Some(actor) if actor.as_deref() == Some(entry.actor.as_str()) => {}
                        Some(actor) => violations.push(format!(
                            "invariant 1: read by {who} on {id} carries actor {actor:?}"
                        )),
                        None => violations.push(format!(
                            "invariant 1: committed read {who} on {id} has no audit row"
                        )),
                    }
                }
            }
            ("update", "missing") => {
                let title = entry
                    .values
                    .as_ref()
                    .and_then(|v| v.get("title"))
                    .and_then(|t| t.as_str())
                    .unwrap_or("");
                if rows
                    .iter()
                    .any(|row| content_title(&row.after) == Some(title))
                {
                    violations.push(format!(
                        "invariant 1: update {who} was answered 404 but its title reached the trail"
                    ));
                }
            }
            _ => {}
        }
    }

    // No orphans: every audit row is claimed by exactly one entry.
    for row in &rows {
        if row.claimed_by.is_none() {
            violations.push(format!(
                "invariant 1: audit row #{} ({} on {}, actor {:?}) matches no writer intent",
                row.pos, row.action, row.auditable_id, row.actor
            ));
        }
    }

    // Invariant 5: update rows tell the truth about what their
    // writer saw. A row's before title must be a title the row
    // genuinely held; chains may fork only on the shared hot rows.
    let mut by_row: BTreeMap<&str, Vec<&AuditRow>> = BTreeMap::new();
    for row in &rows {
        by_row
            .entry(row.auditable_id.as_str())
            .or_default()
            .push(row);
    }
    for (id, rows) in &by_row {
        let titles: BTreeSet<&str> = rows
            .iter()
            .filter_map(|row| content_title(&row.after))
            .collect();
        let updates: Vec<&&AuditRow> = rows.iter().filter(|row| row.action == "update").collect();
        for update in &updates {
            if let Some(before_title) = content_title(&update.before) {
                if !titles.contains(before_title) {
                    violations.push(format!(
                        "invariant 5: update row #{} on {id} claims before title {before_title}, a state the row never held",
                        update.pos
                    ));
                }
            }
        }
        for pair in updates.windows(2) {
            let (prev, next) = (pair[0], pair[1]);
            if let (Some(after), Some(before)) =
                (content_title(&prev.after), content_title(&next.before))
            {
                if after != before && !hot_rows.contains(*id) {
                    violations.push(format!(
                        "invariant 5: update chain on {id} forks outside the hot rows (row #{} saw {before}, previous write left {after})",
                        next.pos
                    ));
                }
            }
        }
        // Final state: the last write decides what notes must hold.
        let last_write = rows.iter().rfind(|row| row.action != "read");
        match last_write {
            Some(row) if row.action == "delete" => {
                if notes_by_id.contains_key(*id) {
                    violations.push(format!(
                        "invariant 5: {id} has a delete as its last write but is still in notes"
                    ));
                }
            }
            Some(row) => match notes_by_id.get(*id) {
                None => violations.push(format!(
                    "invariant 5: {id} was written but is missing from notes"
                )),
                Some(note) => {
                    if let Some(title) = content_title(&row.after) {
                        if note.title != title {
                            violations.push(format!(
                                    "invariant 5: notes title for {id} is {}, the trail's last write says {title}",
                                    note.title
                                ));
                        }
                    }
                    if let Some(body) = content_body(&row.after) {
                        if note.body.as_deref() != Some(body) {
                            violations.push(format!(
                                    "invariant 5: notes body for {id} disagrees with the trail's last write"
                                ));
                        }
                    }
                }
            },
            None => {}
        }
    }
    // Notes rows nobody logged creating (imports are logged too).
    let logged_ids: BTreeSet<&str> = entries
        .iter()
        .flat_map(|e| e.row_ids.iter().map(String::as_str))
        .collect();
    for note in &note_models {
        if !logged_ids.contains(note.id.to_string().as_str()) {
            violations.push(format!(
                "invariant 5: notes row {} ({}) was never logged by any writer",
                note.id, note.title
            ));
        }
    }

    let by_outcome: BTreeMap<&str, usize> = entries.iter().fold(BTreeMap::new(), |mut acc, e| {
        *acc.entry(e.outcome.as_str()).or_default() += 1;
        acc
    });
    println!(
        "verified {} log entries {by_outcome:?} against {} audit rows and {} notes",
        entries.len(),
        rows.len(),
        note_models.len()
    );
    Ok(violations)
}

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(violations) if violations.is_empty() => {
            println!("soak verification: clean");
            ExitCode::from(0)
        }
        Ok(violations) => {
            for violation in &violations {
                println!("VIOLATION: {violation}");
            }
            println!("soak verification: {} violations", violations.len());
            ExitCode::from(1)
        }
        Err(message) => {
            eprintln!("soak verification could not run: {message}");
            ExitCode::from(2)
        }
    }
}
