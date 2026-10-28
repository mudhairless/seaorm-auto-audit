# Cluster soak test

`tests/load.rs` proves the crate's invariants in one process, with
interleavings someone scripted. This proves them across processes,
through the axum middleware, under interleavings nobody scripted:
several API replicas on one shared database, writers firing seeded
random operations at random replicas, and a verifier reconciling
what the writers say they did against the audit trail that resulted.

Local Docker only. It never runs in CI, same deal as the load tests.

## The pieces

- `api` (src/bin/api.rs): a small axum service over the `notes`
  table: create, update, delete, the three audited bulk wrappers,
  marked reads, a transact-fail endpoint that writes and rolls
  back, and an import endpoint that writes inside
  `scope_suspended`. The crate's middleware scopes every request;
  the actor comes from an `x-actor-id` header. Run 2 to 3 replicas.
- `writer` (src/bin/writer.rs): seeded randomized
  operations against random replicas. One actor identity per
  writer, deliberate rollbacks, suspended imports, same-row
  contention over a small pool of hot rows, and unique sentinel
  secrets planted in the redacted (`secret_token`) and excluded
  (`internal_note`) columns. Every operation is logged as JSONL
  (the `OpEntry` shape in src/lib.rs) on a shared volume. The seed
  prints at startup and is settable, so a violating run replays
  exactly.
- `verify` (src/bin/verify.rs): reconciles the writer
  logs against the audits table and the final state of `notes`.

Writers make values unique per operation (a title carries the
writer and the sequence number), so the verifier correlates
exactly instead of guessing which audit row belongs to which
intent. An HTTP 200 from a write endpoint means the write and its
audit row committed together, because each handler commits its
transaction before answering.

## The invariants

1. Every committed, non-suspended write has exactly one audit row,
   carrying the actor that made it.
2. Nothing from a rolled-back transaction is in the trail.
3. Nothing from a suspended scope is in the trail.
4. No sentinel secret appears anywhere in the audits table.
5. Update rows are truthful about what their writer saw; chains
   fork only in the characterized cross-transaction way.

## Running it

```console
$ ./soak.sh up        # Postgres by default; BACKEND=mysql for MySQL
$ ./soak.sh run       # up, plus the three writers (OPS= per writer)
$ ./soak.sh verify    # exit 0 clean, 1 violations, 2 could not run
$ ./soak.sh down
```

The gate for
the 1.0 checklist is an agreed run, an hour or 100k operations,
with zero invariant violations, on Postgres and on MySQL.
