# Worklog — EN.1.C-trigger-dispatch-serve-embedding

## Task 1 — PASSED (1 attempt)
What: Added a Dispatcher in crates/engine-serve/src/dispatch.rs implementing dual-registry (workflow_registry + schema_registry) dispatch keyed by workflow_type, with DispatchError::UnknownWorkflowType for unregistered types, wired into lib.rs via pub mod dispatch, with 3 passing unit tests.
Decisions: Kept Dispatcher::register generic via a boxed WorkflowFactory closure (Box<dyn Fn() -> Workflow + Send + Sync>) rather than adding a NodeRegistry-sharing convenience method, since NodeRegistry/Node trait objects aren't Clone and the task only requires register(...)/dispatch(...) resolution.; Used matches!/manual field comparison instead of assert_eq! against a dispatch() Result because engine_core::Workflow doesn't implement Debug/PartialEq (unwrap_err/assert_eq require those bounds on the Ok variant too).
Validated: gating checks (fast tripwire)
