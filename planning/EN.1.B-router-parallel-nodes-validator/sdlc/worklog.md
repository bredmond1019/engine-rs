# Worklog — EN.1.B-router-parallel-nodes-validator

## Task 1 — PASSED (1 attempt)
What: engine-core now has a Router trait (supertrait of Node) with route(ctx) for runtime next-node selection (including undeclared back-edges), plus a Node::as_router() default hook for registry-based router detection, both re-exported from lib.rs.
Decisions: Added dispatch_route(&dyn Router, &TaskContext) -> Option<String> as the small dispatch helper referenced by the task description, thinly wrapping router.route(ctx); workflow.rs left untouched as required (runner wiring is task 4)
Validated: gating checks (fast tripwire)

## Task 2 — PASSED (1 attempt)
What: Added ParallelNode (fan-out over branch nodes on std::thread::scope, deep-copied TaskContext per branch, deterministic last-write-wins merge of nodes/node_runs by declared branch order) with unit + integration tests; re-exported from engine-core lib.rs.
Decisions: Deterministic merge tie-break = declared branch order (later branch in the Vec passed to ParallelNode::new wins on key collision), documented in module docs and asserted by tests; Used std::thread::scope for fan-out per the spec's preference, avoiding new runtime/thread-pool dependency; First branch NodeError encountered (in declared order) is propagated as ParallelNode::process's own error; no partial merge is returned on branch failure
Validated: gating checks (fast tripwire)

## Task 3 — PASSED (1 attempt)
What: Added WorkflowValidator (BFS reachability, DFS cycle check skipping router edges, non-router fan-out arity guard) with ValidationError enum, re-exported from lib.rs, with 6 unit tests covering valid linear/router-fanout schemas and each rejection case (unreachable node, non-router multi-connection, non-router cycle, router back-edge exemption).
Decisions: Classified a node as router purely via NodeRegistry lookup + Node::as_router().is_some(); an unregistered identity is treated as non-router (its unreachable/unregistered status is caught by the reachability check instead); Validation order is deterministic: fan-out arity checked first (over all nodes), then reachability (BFS), then cycles (DFS) — each pass iterates schema.nodes keys in sorted order for reproducible error reporting; DFS cycle check completely skips walking connections declared out of a router node (rather than just ignoring back-edges), per spec's 'skips edges out of router nodes' language
Validated: gating checks (fast tripwire)

## Task 4 — PASSED (1 attempt)
What: Workflow::run now selects the next node via Router::route(ctx) for router nodes (supporting undeclared runtime back-edges) and connections[0] for plain nodes; a new fallible Workflow::new_validated(registry, schema) runs WorkflowValidator::validate first, while the existing infallible Workflow::new is untouched.
Decisions: Resolved router_next (via as_router()/dispatch_route) before calling node_context, since node_context takes ownership of ctx — this preserves the pre-node-run TaskContext snapshot for route() semantics.; Used a match block instead of expect_err/unwrap_err in the cyclic-rejection test because Workflow intentionally does not implement Debug.
Validated: gating checks (fast tripwire)

## Task 5 — PASSED (1 attempt)
What: Task 5 (Validate) confirmed cargo fmt --check, cargo clippy -- -D warnings, cargo test, and cargo build --release all pass with no code changes needed.
Validated: gating checks (fast tripwire)

## Docs
Patched: docs/architecture.md

## Wrap-up — PASS
Next: Merge EN.1.B-router-parallel-nodes-validator-flow into main, then define the next Phase 1 block
