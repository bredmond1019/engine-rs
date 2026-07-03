# Worklog — EN.1.A-node-trait-workflow-runner

## Task 1 — PASSED (1 attempt)
What: engine-core now defines the Node trait (process/name) and a NodeRegistry (HashMap<String, Box<dyn Node>>) with lookup, backed by engine-contract's TaskContext, with unit tests for transformation and identity-key matching.
Decisions: NodeError is a simple struct wrapping a String message, implementing Display + std::error::Error, rather than an enum — task 1 scope only needs a minimal failure carrier; richer variants can be added later if needed.; Node trait bounds Send + Sync so boxed trait objects work cleanly in a registry/runner context used across async boundaries later.; Dropped the crate_name() placeholder function and its test since nothing outside engine-core referenced it.
Validated: gating checks (fast tripwire)

## Task 2 — PASSED (1 attempt)
What: engine-core now defines WorkflowSchema/NodeConfig with helpers to resolve the start node and each node's connections[0] next-node, covered by unit tests on a linear 3-node schema.
Decisions: NodeConfig::next() returns connections.first() as &str, matching the 'connections[0] only' scope for this block; WorkflowSchema stores nodes as HashMap<String, NodeConfig> keyed by node identity for O(1) start/next lookups
Validated: gating checks (fast tripwire)

## Task 3 — PASSED (1 attempt)
What: Added the Workflow pointer-walk runner (crates/engine-core/src/workflow.rs) with the on_progress persistence seam and node_context envelope, seeding all nodes PENDING before the walk, stamping RUNNING/SUCCESS/FAILED transitions with timing, and halting on node failure; wired into lib.rs and covered by unit tests.
Decisions: Node::process consumes and only returns TaskContext on Ok, so node_context clones ctx before calling process to have a base context available for stamping the FAILED transition on Err (since NodeError carries no context back).; Used Rc<RefCell<Vec<TaskContext>>> in tests to capture on_progress snapshots, avoiding a borrow conflict between the FnMut closure (borrowed mutably during run) and post-call assertions on the captured Vec.; WorkflowError (distinct from NodeError) is returned only for graph-shape issues like an unregistered node identity; a node's own failure is captured in NodeRun and does not short-circuit run() with an Err — the accumulated TaskContext is still returned Ok.
Validated: gating checks (fast tripwire)

## Task 4 — PASSED (1 attempt)
What: Added the fixture 3-node linear workflow integration test (workflow_runner.rs) covering full-success PENDING->RUNNING->SUCCESS transitions, initial on_progress PENDING snapshot, and a middle-node failure that halts the walk before the third node runs.
Decisions: Used identity names start_node/node2/node3 to match schema.rs conventions while keeping the linear 3-node fixture clear; Asserted the initial on_progress snapshot's TaskContext::nodes is empty as an extra check that the PENDING seed snapshot precedes any node execution
Validated: gating checks (fast tripwire)

## Task 5 — PASSED (1 attempt)
What: Ran the spec's four validation commands (cargo fmt --check, cargo clippy -- -D warnings, cargo test, cargo build --release); all pass with no code changes needed.
Decisions: Task 5 is validation-only (no files listed); confirmed working tree was already clean/passing from prior tasks, so no commit was made.
Validated: gating checks (fast tripwire)
