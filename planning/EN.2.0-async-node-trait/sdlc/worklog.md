# Worklog — EN.2.0-async-node-trait

```
## Task 1 — PASSED (1 attempt)
What: Added async-trait and futures as workspace dependencies, wired into engine-core (real deps) and engine-serve (async-trait only), with all four gates (fmt, clippy -D warnings, test, build --release) passing unchanged.
Validated: gating checks (fast tripwire)
```

## Task 2 — PASSED (1 attempt)
What: Node::process is now async fn via #[async_trait::async_trait]; Workflow::run/node_context are async; ParallelNode fans out via futures::future::join_all instead of std::thread::scope; engine-serve's post_events awaits workflow.run directly with web::block removed.
Decisions: Kept the module doc comment in http.rs referencing web::block only in the removal-rationale comment, not in code, per the acceptance criteria that web::block is gone from the file's logic; router_route_dispatch_matches_runner_behavior and cyclic_non_router_workflow_is_rejected_by_new_validated in validator.rs stayed #[test] (not tokio::test) since neither calls .process()/.run(), consistent with minimal-diff migration
Validated: gating checks (fast tripwire)

## Task 3 — PASSED (1 attempt)
What: Confirmed all four gated checks (cargo fmt --check, cargo clippy -D warnings, cargo test, cargo build --release) pass against the Task 2 async-Node conversion, and web::block no longer appears as a live call in crates/engine-serve/src/http.rs (only in a comment).
Decisions: Task 3 has no files listed in tasks.json and is purely a validation gate — no code changes were made and no commit was created since the working tree was already clean after Task 2.
Validated: gating checks (fast tripwire)

## Docs
Patched: docs/architecture.md
