# Working on seaorm-auto-audit

Notes for agents (and humans) changing this crate. The README is the
user documentation; this file is only about how changes get made here.

## The gate

Every change must pass what CI runs (`.github/workflows/ci.yml`):

    cargo fmt --all -- --check
    cargo clippy --all-targets --all-features -- -D warnings
    cargo check --no-default-features
    cargo test --all-features
    RUSTDOCFLAGS=-D warnings cargo doc --all-features --no-deps

plus the MSRV job: `cargo check --all-features` on Rust 1.94.0, the
crate's `rust-version`. Do not use std or dependency APIs newer than
1.94.0 supports. The Postgres and MySQL suites skip without their
database URLs locally; CI runs them for real.

## Rules that are easy to break

- Load tests never run in CI. `tests/load.rs` is all `#[ignore]`d;
  `./load_test.sh` is the only runner.
- Batched audit writes and read-backs slice by `STATEMENT_CHUNK`
  (256, `src/lib.rs`). SQLite allows 32,766 variables per statement;
  a new batched path joins the slicing, it does not get its own
  constant.
- Redaction is a security property, not a display choice. A new write
  path applies `Auditable::redacted_columns` and
  `excluded_columns`, and joins the matrix in `tests/redaction.rs`
  in the same change.
- Snapshot keys are database column names, never serde field names
  (see `stash::to_json`). Renaming a column in a test entity is how
  this gets caught.
- A schema change touches all of: `sql/setup.sqlite.sql`,
  `sql/setup.postgres.sql`, `sql/setup.mysql.sql`, the entity in
  `src/audits.rs`, and the tests that build the table.
- Rustdoc links: use full `crate::` paths, and disambiguate when a
  name is both a function and a module. The doc build denies
  warnings, so a broken link fails the gate.
- Changelog entries go under `[Unreleased]` in `CHANGELOG.md`. There
  are no GitHub Releases; crates.io is the distribution channel.
- The agent skills under `skills/` declare the crate versions they
  apply to in their frontmatter. An API change moves those ranges in
  the same change.
