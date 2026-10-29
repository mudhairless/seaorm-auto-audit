---
name: find-unmarked-reads
description: Enumerate the fetch call sites of auditable entities in a codebase, classify which ones are user-facing reads, and mark the unmarked ones with record_read / record_reads from seaorm-auto-audit.
applies_to: seaorm-auto-audit >=0.9, <2.0
revision: 1
---

# Find unmarked reads

seaorm-auto-audit audits writes through SeaORM lifecycle hooks. Reads
have no hooks, so a read is logged only when the application marks it
with `record_read` (one row) or `record_reads` (a page of rows). A
call site nobody marked is a silent gap in the access log: nothing
fails, nothing warns. That is why the enumeration below is the
deliverable of this skill. The edits are the easy part.

## Step 0: verify the crate version before anything else

1. Resolve the version of `seaorm-auto-audit` the project actually
   uses from `Cargo.lock` (the requirement string in `Cargo.toml` is
   not the ground truth). If it falls outside the range in this
   file's frontmatter, stop and report that. Do not improvise from
   stale facts.
2. Spot-check one load-bearing fact against that version's own
   docs on docs.rs: the signature of `record_read`. If the signature
   there differs from the one in this skill, stop and report that
   too. A wrong skill should fail here, not in the user's code.

The signatures this skill was written against:

```rust
seaorm_auto_audit::record_read::<users::Entity>(
    &db, &model, Some("why this read happened"),
    Some(serde_json::json!({"name": null, "email": null})),
).await?;

seaorm_auto_audit::record_reads::<users::Entity>(
    &db, &page_of_models, Some("search results"), Some(fetched),
).await?;
```

## Step 1: name the auditable entities

Search the source tree for `impl Auditable for` and
`impl_audit_hooks!`. The types named there are the auditable
entities. Everything else in the codebase is out of scope.

## Step 2: enumerate every fetch call site

Search the whole source tree for read call sites on those entities:
`::find(`, `find_by_id`, `find_also_related`, `find_with_related`,
`find_by_statement`, and the terminal calls `.one(`, `.all(`,
`.paginate(` on query builders over an auditable entity. Follow
indirection: a fetch inside a helper function counts at the helper,
and every caller of the helper inherits its classification. A site
the search cannot reach is a site the access log cannot see either,
so completeness of this list matters more than speed.

## Step 3: classify each site

- **User-facing**: the fetched data leaves the process. An HTTP
  response, a rendered page, an export, a report, an email, a
  message. These are the reads an access log exists for.
- **Internal**: the data stays inside. An existence check before a
  write, input to a join or a calculation, a cache fill, background
  bookkeeping.
- **Already marked**: a `record_read` or `record_reads` call covers
  the site.

When a site is genuinely ambiguous, list it as unclassified and ask
the user. Never guess a sensitive read into the internal bucket to
make the list shorter.

## Step 4: present the enumeration before editing

The deliverable is a table: file and line, entity, what the site
fetches, its classification, and whether it is marked. Show it to
the user and get agreement on the user-facing set before changing
any code.

## Step 5: mark the user-facing sites

- One row fetched for display: `record_read` right after the fetch.
- A list: `record_reads` once, with the page actually displayed,
  not every match and not one call per row.
- The call must run inside the audit scope (the crate's axum
  middleware, or an explicit `seaorm_auto_audit::scope(..)`). A read
  record outside a scope fails with the crate's `NO_SCOPE` error.
- `comment` is the caller's words for why the read happened
  ("opened from support ticket 412", "search results"). It is what
  tells a deliberate viewing apart from a row surfacing in a list.
- `fetched` is a JSON object naming what the read exposed. Its keys
  are database column names, which differ from Rust field names
  when a column is renamed. Values are the caller's choice: the
  observed values, or `null` for columns-only. Name only exposed
  columns. The crate strips any key named by `redacted_columns()`
  or `excluded_columns()` before storing, but treat the map as a
  statement of exposure and build it from the exposed set.
- Keep the surrounding error handling style. These calls return
  `Result<(), DbErr>` like the crate's other audit writes.

## Step 6: verify

Build the project and run its tests. Then re-run the Step 2
enumeration and show that every user-facing site is now marked.
Report the final table, including the sites classified internal and
unclassified, so the user can see what was deliberately left out.
