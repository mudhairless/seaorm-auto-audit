# Security Policy

## Supported versions

| Version | Supported |
| ------- | --------- |
| 0.9.x   | Yes       |
| < 0.9   | No        |

This crate is pre-1.0 and maintained by one person. Fixes land in the
newest 0.x release; there are no backports to older lines.

## Reporting a vulnerability

Please do not open a public issue for a security problem. Report it
privately through GitHub:

https://github.com/mudhairless/seaorm-auto-audit/security/advisories/new

Include what you found, the version you found it in, and the smallest
reproduction you can put together. A failing test against a test
database is ideal, but a clear description of the steps is enough.

Expect an acknowledgement within a few days. This is a solo project,
so a fix may take longer than the acknowledgement; if the issue is
serious, say so in the report and it moves to the front of the queue.

## Scope

In scope:

- The `seaorm-auto-audit` library and the `seaorm-auto-audit-check`
  binary.
- Anything that puts data in an audit row that should not be there.
  The redaction and exclusion rules are a security feature of this
  crate: if a column named by `redacted_columns()` or
  `excluded_columns()` reaches the `audits` table through any of the
  crate's audited write paths (the lifecycle hooks, the bulk wrappers,
  the read helpers), that is a vulnerability, not just a bug. The
  untyped `write_record` / `write_records` helpers are the documented
  exception: they store records exactly as given, so what goes through
  them is the caller's responsibility.
- Audit rows written for the wrong actor, or written by the hooks or
  the bulk wrappers when no audit scope was open.

Out of scope:

- The example in `examples/` and the documentation.
- Vulnerabilities in dependencies that do not affect this crate's
  behavior. Report those upstream; if an update here is needed once
  upstream fixes them, a regular issue is fine.
- Findings from the load tests in `tests/load.rs`. Those characterize
  known behavior, which the README's Gotchas section describes.
