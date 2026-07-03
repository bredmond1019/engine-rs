# Worklog — EN.1.A-node-trait-workflow-runner

## Task 1 — PASSED (1 attempt)
What: engine-core now defines the Node trait (process/name) and a NodeRegistry (HashMap<String, Box<dyn Node>>) with lookup, backed by engine-contract's TaskContext, with unit tests for transformation and identity-key matching.
Decisions: NodeError is a simple struct wrapping a String message, implementing Display + std::error::Error, rather than an enum — task 1 scope only needs a minimal failure carrier; richer variants can be added later if needed.; Node trait bounds Send + Sync so boxed trait objects work cleanly in a registry/runner context used across async boundaries later.; Dropped the crate_name() placeholder function and its test since nothing outside engine-core referenced it.
Validated: gating checks (fast tripwire)
