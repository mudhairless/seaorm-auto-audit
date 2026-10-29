---
name: audit-usage-gotchas
description: Review a codebase's use of seaorm-auto-audit against the mistakes the coverage checker cannot see: run the checker binary first, then check scopes, spawned tasks, same-row interleaving, graph saves, and bulk filter indexes by hand.
applies_to: seaorm-auto-audit >=0.9, <2.0
revision: 2
---

# Audit usage gotchas review

The crate's checker binary finds the mechanical mistakes: write
call sites that bypass the audit hooks. The mistakes in this skill
are semantic. They compile, they pass a call-site scan, and they
still produce a wrong or missing audit trail. This review covers
both, in that order.

## Step 0: verify the crate version before anything else

1. Resolve the version of `seaorm-auto-audit` the project actually
   uses from `Cargo.lock` (the requirement string in `Cargo.toml`
   is not the ground truth). If it falls outside the range in this
   file's frontmatter, stop and report that. Do not improvise from
   stale facts.
2. Spot-check one load-bearing fact against that version's own
   docs on docs.rs: the exit codes of the `seaorm-auto-audit-check`
   binary, as stated in the README for that version. If they differ
   from the table below, stop and report that too.

## Step 1: run the checker

```console
$ cargo run -p seaorm-auto-audit --bin seaorm-auto-audit-check -- <source roots>
```

(With the binary installed, `seaorm-auto-audit-check <source roots>`
is the same thing.) The exit code is the contract:

| code | meaning |
|------|---------|
| 0 | clean: the roots were scanned, no forbidden call site found |
| 1 | at least one forbidden call site was found |
| 2 | the check itself failed: bad arguments, a root that does not exist or cannot be read, or a root with no Rust sources in it |

Treat exit code 2 as a failed review, never as a pass. Report the
checker's findings verbatim before moving on; they are the
mechanical layer of the report.

## Step 2: the semantic checks

For each check: where to look, and what correct usage looks like.

### Writes on a path that never opens a scope

An audited write outside an audit scope fails at runtime with the
crate's `NO_SCOPE` error rather than writing a half-empty audit
row. Find write call sites on auditable entities (ActiveModel
`insert` / `update` / `save` / `delete`, the crate's bulk wrappers)
and trace each one's execution path back to a scope: the crate's
axum middleware on the route, or an explicit
`seaorm_auto_audit::scope(..)` around the job. A write whose path
has neither is a finding, even if nobody has hit the error yet.

### Spawned tasks that do not re-scope

The audit context is task-local and does not cross `tokio::spawn`.
Search for `tokio::spawn` (and detached task spawns generally)
whose body touches an auditable entity. Correct usage re-scopes
inside the spawned future with `seaorm_auto_audit::scope(..)`,
passing the actor captured before the spawn. The mirror-image
mistake: a spawn inside `scope_suspended` loses the suspension,
so its writes are audited as usual and fail if no actor scope is
there. Neither is silently wrong; both surprise.

### Same-row writes interleaved in one scope

Two futures on one task that both write the same row concurrently
(`join!`, `try_join!`, parallel branches of one request) are not
supported: the second `before_save` overwrites the first write's
parked before-state, and the second `after_save` then fails with
"no before-state". Sequential writes to the same row are fine,
including graph saves. Flag only same-entity writes on concurrent
branches of one scope.

### Graph saves outside a transaction

A nested `builder().save()` runs the hooks per model, on the same
connection, with the same actor. Outside a transaction, a failure
mid-graph leaves partial writes and partial audit rows behind.
Find `save()` calls on relation builders for auditable entities
and check each one runs inside a transaction (`db.begin()`).

### update_without_returning on an auditable entity

This SeaORM method runs `before_save` but never `after_save`, so
the write produces no audit row. It is in the checker's forbidden
list for auditable tables; if the checker was run with an
`--allow` exemption covering the file, flag the site here anyway
and let the user judge the exemption.

### Suspension used as a routine path

`scope_suspended` exists for seeders, imports, and backfills, and
is deliberately a named, greppable scope. Flag any use inside
ordinary request handling. Not a finding: the bulk wrappers keep
their own guards under suspension (`update_many` still refuses to
change a primary key, `insert_many` on MySQL still needs explicit
keys). That is correct behavior, not a bug to report.

### Bulk filters on unindexed columns

The bulk wrappers read the rows their filter matches before
writing, so an unindexed filter column means full scans under
lock. On MySQL, concurrent bulk operations over an unindexed
column take gap locks that can deadlock (error 1213), and one
transaction is rolled back. When a bulk call site filters on a
column, look for an index covering that column in the project's
schema or migrations. No index is a finding: a latency problem on
any backend, and an availability problem on MySQL under
concurrency.

## Step 3: report

Group the findings: checker output first, then the semantic ones,
each with file and line, what goes wrong, and the shape of the
fix. Propose edits; do not silently change behavior. Where a
pattern is widespread, say so with a count instead of listing
every instance.
