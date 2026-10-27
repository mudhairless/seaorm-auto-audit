# Changelog

All notable changes to seaorm-auto-audit are recorded here. The format
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the
project aims to follow semantic versioning while it is pre-1.0: a minor
version bump may carry a deliberate behavior change, and such changes are
listed under Changed.

## [Unreleased]

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
