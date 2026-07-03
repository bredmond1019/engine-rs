# Worklog — EN.1.B-router-parallel-nodes-validator

## Task 1 — PASSED (1 attempt)
What: engine-core now has a Router trait (supertrait of Node) with route(ctx) for runtime next-node selection (including undeclared back-edges), plus a Node::as_router() default hook for registry-based router detection, both re-exported from lib.rs.
Decisions: Added dispatch_route(&dyn Router, &TaskContext) -> Option<String> as the small dispatch helper referenced by the task description, thinly wrapping router.route(ctx); workflow.rs left untouched as required (runner wiring is task 4)
Validated: gating checks (fast tripwire)

## Task 2 — PASSED (1 attempt)
What: Added ParallelNode (fan-out over branch nodes on std::thread::scope, deep-copied TaskContext per branch, deterministic last-write-wins merge of nodes/node_runs by declared branch order) with unit + integration tests; re-exported from engine-core lib.rs.
Decisions: Deterministic merge tie-break = declared branch order (later branch in the Vec passed to ParallelNode::new wins on key collision), documented in module docs and asserted by tests; Used std::thread::scope for fan-out per the spec's preference, avoiding new runtime/thread-pool dependency; First branch NodeError encountered (in declared order) is propagated as ParallelNode::process's own error; no partial merge is returned on branch failure
Validated: gating checks (fast tripwire)
