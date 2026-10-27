# Changelog

All notable changes to seaorm-auto-audit are recorded here. The format
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the
project aims to follow semantic versioning while it is pre-1.0: a minor
version bump may carry a deliberate behavior change, and such changes are
listed under Changed.

## [Unreleased]

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
