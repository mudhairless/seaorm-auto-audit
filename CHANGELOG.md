# Changelog

All notable changes to seaorm-auto-audit are recorded here. The format
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). From
1.0 the project follows Semantic Versioning; before 1.0, a minor version
bump could carry a deliberate behavior change, listed under Changed.

## [Unreleased]

## [1.0.0] - 2026-10-28

First stable release. The public API as reviewed for 1.0 is the
contract under Semantic Versioning from here on. The entries below
are the deliberate changes made during that review, relative to
0.9.0, followed by everything else that landed since.

### Fixed

- `prune` in `Delete` mode now slices its in-transaction re-read and its
  delete by id, reusing the crate's 256-row statement chunk, so a prune
  selecting more than about 32k rows no longer fails on SQLite with too
  many SQL variables. The rows returned are still exactly the rows the
  delete removed, in the same oldest-first order.

- Snapshots are keyed by database column name, and `redacted_columns()`
  / `excluded_columns()` are matched against those names, as the docs
  always said. Snapshots used to be built by serializing the model, so
  the keys were serde names: on a model whose column name differs from
  its field or serde name (a `column_name` override, a serde rename),
  listing the column name silently redacted nothing and the value was
  written into the audit table. Projects that listed a renamed column
  by its field or serde name must switch the entry to the column name.

### Changed

- `Checker::scan` returns `Result<Vec<Violation>, ScanError>` instead of
  a bare vector. A root that cannot be read, a file in the tree that
  cannot be read, and a root that yields no Rust source files at all
  are now errors rather than silent empty results: the checker's job is
  gating a build, and a gate that passes having checked nothing is
  worse than no gate. The `seaorm-auto-audit-check` binary exits 2 in
  those cases. Callers that treated "missing or empty input" as "clean"
  were relying on the bug.

- The axum middleware no longer records the request's query string in
  `audits.url`: only `scheme://host/path` is stored. Query strings
  carry whatever a client put there, including access tokens and
  password-reset links, and an audit trail should not collect them.
  The `url` field keeps its name and its meaning otherwise.

- `AuditRecord`'s fields are renamed to match the table they land in:
  `table` is now `auditable_table` and `id` is now `auditable_id`
  (the constructor's parameters follow).
- `AuditContext` is no longer public. It was buildable but could not
  be passed to any public function; `scope` and `scope_with_request`
  remain the way in.
- The middleware's fallback client-address extension is renamed from
  `ClientIp` to `ClientIpExtension`; it shared a name with
  `axum_client_ip::ClientIp`, which the same module imports.
- `PrunePolicy` no longer implements `Default`: the default policy
  (neither rule set) is always refused by `prune`, so the derive was
  a trap. Build one with `keep_latest`, `older_than`, or a struct
  literal.
- `AuditAction` and `PruneMode` are now `#[non_exhaustive]`.
- `audits::Model::modified` returns its changes sorted by column
  name, independent of the stored JSON's key order.
- `record_read` / `record_reads` refuse a `fetched` payload that is
  not a JSON object, instead of storing it unstripped (the redaction
  stripping works by key, and a non-object has no keys to strip).
- The MySQL setup script declares `auditable_id` as TEXT, like the
  other backends and the entity, instead of VARCHAR(255); the
  (table, id) index keeps a 255-character prefix on the id.
- The re-export of the `async_trait` macro is now `#[doc(hidden)]`;
  it was never part of the documented surface.

## [0.9.0] - 2026-10-27

### Changed

- The `audits` table gains a nullable `comment` column, and its action
  check now allows `read`. Existing users must add the column to their
  `audits` table (`ALTER TABLE audits ADD COLUMN comment TEXT`, in their
  database's spelling) and widen the action check to include `'read'`
  before using the read-recording helpers; the shipped `sql/setup.*.sql`
  files carry the new shape for fresh installs.

### Added

- `record_read` / `record_reads`: record a read of an audited row as an
  audit event (action `read`), for applications that mark sensitive reads
  themselves, since SeaORM has no read hooks. A scope is required. The
  record stores no snapshot; an optional comment (the new `comment`
  column) says why the read happened, and an optional caller-supplied
  `fetched` object is stored as the after content, its keys naming the
  exposed columns, with keys named by `redacted_columns()` or
  `excluded_columns()` stripped before storing. `modified()` rejects
  read records.


- `prune(db, policy, mode)`: standalone pruning of the `audits` table.
  A `PrunePolicy` keeps the latest N audits per (table, id) and/or drops
  audits older than an age; the rules combine as a union, and a policy
  with neither is refused. `PruneMode::DryRun` returns the rows the
  policy selects and deletes nothing; `PruneMode::Delete` removes them
  in a transaction and returns the rows removed. Nothing prunes on write
  and nothing is scheduled.


- `Auditable::excluded_columns()`: columns left out of audit records
  entirely, with no marker. An update that touches only excluded columns
  writes no audit row. Distinct from `redacted_columns()`, whose changes
  are still recorded as a marker; a column listed in both is excluded.


- `audits::Model::modified()`: the per-column changes a stored audit record
  describes, as `ColumnChange` values (column name, before value, after
  value). Update records report the columns in their stored pair; update
  records written by 0.x versions, which hold full snapshots, have their
  unchanged columns filtered out, while a column redacted on both sides is
  still reported, since the markers mean it changed. Records whose action is
  not create, update or delete are rejected.


### Changed

- Update audit rows now store only the columns that changed, on both the
  before and after sides, instead of the full row. An update that changed
  nothing writes no audit row. Creates and deletes still store the full
  row.

### Added

- `scope_suspended`, an explicit scope in which audited writes proceed
  without writing audit rows, for seeders, imports and backfills.

### Fixed

- The bulk wrappers read rows back by primary key in slices of 256. The
  read-back condition is an `OR` of one `AND` per row, so its expression
  tree grows with the row count, and past roughly a thousand rows SQLite
  refused it with "Expression tree is too large", failing the whole bulk
  operation. Larger batches now take several smaller read-backs.
- Batch writes go in slices of 256 rows per statement. One statement
  binds a variable per column per row, and past a few thousand wide rows
  that exceeds the per-statement variable limit (32,766 on SQLite,
  65,535 on Postgres and MySQL), failing with "too many SQL variables".
  This applies to the audited `insert_many` and to every audit batch
  written through `write_records`.


## [0.4.1] - 2026-10-26

### Fixed

- The `seaorm-auto-audit-check` binary now refuses a scan root that does
  not exist, exiting with code 2 and an error message, instead of
  reporting success over zero files. A mistyped path in a CI gate no
  longer passes silently.

### Documentation

- The README gained an install section with the compatibility line
  (SeaORM 2.0, Rust 1.94.0 or later), a worked example of an audit row,
  and crates.io and docs.rs badges. This changelog was added.

## [0.4.0] - 2026-10-26

### Changed

- `update_many` now rejects a `values` model that sets a primary-key
  column, instead of writing audit rows keyed by a key the row no longer
  has. The bulk wrappers share one batched audit write per call.

### Added

- `hooks::write_records`, writing a slice of prepared audit records in one
  pass. The bulk wrappers use it; the single-row path is unchanged.

### Fixed

- The coverage checker no longer follows symlink cycles forever when
  scanning a directory tree.
- `insert_many` with no models returns an empty vector instead of an
  error.

## [0.3.1] - 2026-10-26

### Fixed

- The docs.rs build now enables the `axum` feature, so the middleware
  module renders in the published documentation.
- When a configured `ClientIpSource` header is missing or not an address,
  the audit row records no address instead of silently falling back to
  the connection's peer address.
- Recorded request URLs take their scheme from `X-Forwarded-Proto` when
  it holds `http` or `https`.

## [0.3.0] - 2026-10-26

### Changed

- The minimum supported Rust version is 1.94.0, matching SeaORM 2.0.4.

## [0.2.0] - 2026-10-25

### Changed

- `AuditId` converts identifiers to a canonical text form; the audits
  table stores both the audited row's key and the acting user's id as
  text, so one table covers uuid, integer and string keys.
- Audit row ids are generated as UUIDv7 and sort by creation time.

## [0.1.0] - 2026-10-23

### Added

- First release. Lifecycle-hook auditing for SeaORM entities with
  before/after snapshots, a task-local actor and request metadata,
  credential redaction that still records that a redacted column
  changed, audited bulk wrappers (`insert_many`, `update_many`,
  `delete_many`, `delete_by_id`), axum middleware for request scoping,
  and the `seaorm-auto-audit-check` binary that flags write sites which
  bypass the hooks.
