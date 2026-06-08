# Cortex v4.1 Architecture Note: Borrow Strategy and Runtime Guarantees

## Scope
This note captures the v4.1 runtime boundaries for MCP dispatch, memory access, and reliability checks.

## Borrow strategy
- `Store` owns a single SQLite connection handle and exposes immutable shared access via `conn()`.
- Tool handlers operate with short-lived statement scopes to avoid overlapping mutable borrows of prepared statements.
- Retrieval-heavy handlers convert query iterators into owned vectors before running secondary writes (for example, evidence updates) to prevent nested borrow conflicts.
- Status and doctor reporting rely on read-only queries and avoid long-lived transactions.

## Reliability guarantees
- Schema migration remains additive and idempotent.
- MCP tool list remains the contract surface for wrappers and smoke validation.
- Response cache excludes non-deterministic or path-sensitive tools via explicit `UNCACHEABLE` entries.
- Query gaps are tracked as first-class telemetry (`query_gap_log`) and surfaced in status/doctor outputs.

## Evidence weighting guarantees
- Outcome evidence is session-scoped and idempotent through `outcome_applied_session`.
- Weighted updates derive from `session_retrieval_log` + `outcome_log` and only touch known pattern rows.
- Survival rate recalculation remains deterministic:
  - if `use_count + reverted_count == 0`, survival is `1.0`
  - else survival is `use_count / (use_count + reverted_count)`

## Benchmark harness guarantees
- Syntax benchmark reports latency and symbol-shape coverage using local catalog/index data.
- Dependency benchmark reports graph traversal latency and optional corpus precision.
- Both harnesses run read-only and can be used in CI or first-run validation scripts.

## Operator guidance
- Use `cortex --format json status --full` to inspect query-gap hotspots.
- Use `cortex --format json doctor workflow` to verify health checks include query-gap telemetry.
- Use `cortex benchmark --target syntax` and `cortex benchmark --target dependency` for repeatable local measurements.
- Use `cortex outcome-apply --session-id <id>` after logging outcomes for a session.
