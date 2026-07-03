# Worklog — EN.1.B-router-parallel-nodes-validator

## Task 1 — PASSED (1 attempt)
What: engine-core now has a Router trait (supertrait of Node) with route(ctx) for runtime next-node selection (including undeclared back-edges), plus a Node::as_router() default hook for registry-based router detection, both re-exported from lib.rs.
Decisions: Added dispatch_route(&dyn Router, &TaskContext) -> Option<String> as the small dispatch helper referenced by the task description, thinly wrapping router.route(ctx); workflow.rs left untouched as required (runner wiring is task 4)
Validated: gating checks (fast tripwire)
