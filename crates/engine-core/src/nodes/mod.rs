//! Reusable `Node` implementations that wrap external transports (as opposed to
//! `crate::node`, which defines the `Node` trait/registry themselves).
//!
//! `claude_code_step` (`EN.2.A`) wires the `core/claude-code-rs` SDK's async
//! `execute()` into a `Node`, mapping its `Outcome` into `NodeRun`/`TaskContext`.
//!
//! `openai_compat_transport` (`EN.3.C` task 5) builds a `ModelTransport` for
//! the `local` model tier: an OpenAI-compatible HTTP transport with the same
//! signature as `claude_code_rs::execute`, so it slots into
//! `ClaudeCodeStep::with_transport` (or any task-loop node's own
//! `with_transport`) with zero changes to `ClaudeCodeStep` itself.
//!
//! `http_post` (`EN.4.C` task 2) is the injectable engine-brain HTTP-POST
//! seam `PersistToBrainNode` calls to push the finished `AutomationRoadmap`
//! to the brain ingest endpoint (Synapse's `POST /ingest/*`, `OR.Q`) — a
//! `reqwest`-backed live implementation plus a test stub that records the
//! last payload it was handed.
//!
//! `channel_transport` (`EN.6.A` task 1) is the injectable egress seam
//! `ActionDispatchNode` calls to deliver a run's outbound actions (digest
//! replies, workflow-trigger chaining) to the channel that originated it —
//! mirrors `http_post`'s trait + live impl + recording stub shape.
//!
//! `email` (`EN.6.B` task 2) is `EmailChannelTransport` — the
//! `ChannelTransport` impl that sends outbound mail through the Resend
//! HTTP API over the `http_post` seam, threading replies via `ReplyContext`
//! and echoing `opportunity_slug` metadata onto Resend `tags` for
//! bounce/delivery correlation. `EN.6.B` tasks 4-5 extend the module with
//! inbound-mail parsing and delivery/bounce event mapping.
//!
//! `doc_materializer` (`EN.7.A` task 3) is the injectable doc-materialize
//! seam `MaterializeDocNode` (`EN.7.A` task 4) calls to write a
//! `BrainDocModel`-shaped artifact into the Brain corpus as a source `.md`
//! document via `mev`/`okf-core` in-process — a live `mev`-backed
//! implementation plus a test stub that records the last call it was
//! handed, mirroring `http_post`'s shape. `EN.7.B` extends the seam with a
//! second operation, `edit_opportunity`, over `OpportunityEdit`'s
//! `SetStage`/`AddAction` variants — the write half of the opportunity-edit
//! micro-workflows, reusing the same `MaterializeOutcome` result shape.
//!
//! `materialize_doc` (`EN.7.A` task 4) is `MaterializeDocNode` itself — the
//! generic, reusable node that reads a `BrainDocModel`-shaped artifact out
//! of an upstream node's `TaskContext` (or `ctx.event`) and calls the
//! `doc_materializer` seam to write it into the Brain corpus. It lives here
//! rather than under a `workflows::*` module because every future pipeline
//! appends it; `EN.7.B` wires concrete instances into specific graphs.
//!
//! `opportunity_edit` (`EN.7.B` task 3) is `OpportunityEditNode` — the
//! generic node that drives one `doc_materializer::OpportunityEdit`
//! operation (`set-stage` | `add-action`), configured with WHICH edit it
//! performs and reading that edit's arguments off `ctx.event`. Lives here
//! for the same reason `materialize_doc` does; `EN.7.B` task 5 wires two
//! `with_identity`-distinguished instances into the `set-stage` /
//! `add-action` single-node micro-workflows.
//!
//! `merge_contacts` (`EN.4.E` task 7) is `MergeContactsNode` — the terminal
//! node that collects the contacts a `RESEARCH_AGENT` run surfaced (company
//! brief top-level `contacts[]`, or per-lead `contacts[]` flattened across a
//! prospecting result's `prospects[]`) and merges them into the opportunity
//! `MaterializeDocNode` just wrote, via `doc_materializer::OpportunityEdit::
//! MergeContacts`. Lives here for the same reason its siblings do; the
//! `RESEARCH_AGENT` graph wires one instance in after `MaterializeDocNode`
//! on both research branches.
//!
//! `harvest_gate` (`EN.7.C` task 1) is the generic materialize→harvest gate
//! primitive: `HarvestMode` (`off` | `in_process` | `approval`, default
//! `off`), the resolved per-run `HarvestGate` a node holds, and
//! `pending_harvest_record` — the one constructor for the deferred-harvest
//! record shape shared by `PersistToBrainNode` (task 4, the node that defers)
//! and `HarvestApproveNode` (task 6, the node that completes one). Lives
//! under `nodes/`, not `workflows::content_pipeline`, because every pipeline
//! that materializes a doc and pushes it to Synapse's ingest endpoint
//! inherits this gate.
//!
//! `harvest_approve` (`EN.7.C` task 6) is `HarvestApproveNode` — the
//! completion half of `HarvestMode::Approval`: reads a pending-harvest
//! record off `ctx.event` and POSTs its `payload` to its `url` over the
//! injectable `HttpPost` seam, verbatim, so the eventual push is
//! byte-identical to what an `in_process` push would have sent. Wired into
//! the single-node `HARVEST_APPROVE` micro-workflow
//! (`crate::workflows::harvest_approve`).
//!
//! `suspend` (`EN.6.F` task 5) is `SuspendNode` — the workflow-authored half
//! of suspend/resume. It only *requests* suspension (via
//! `crate::suspend::request_suspension`); `Workflow::walk` (`EN.6.F` task 4)
//! is what actually stops the walk and picks the resume pointer.
//! `enabled: false` (the default) is an in-place no-op, mirroring
//! `MaterializeDocNode::with_enabled`.
//!
//! `fan_out` (`EN.6.G` task 1) is `FanOutNode` — constructs N
//! `with_identity`-wrapped instances of the same underlying node type from
//! one builder closure and delegates to `crate::parallel::ParallelNode` to
//! run them concurrently with no last-write-wins collision. Also carries
//! the `impl Node for Box<dyn Node>` forwarding impl that makes
//! `.with_identity()` callable on a boxed, type-erased node.
//!
//! `aggregate` (`EN.6.G` task 1) is `AggregateNode` — joins the N
//! `ctx.nodes` entries a `FanOutNode` produced into one
//! deterministically-ordered `Vec<serde_json::Value>`, ordered by the
//! caller's declared source-identity list rather than `HashMap` iteration
//! order.

pub mod aggregate;
pub mod channel_transport;
pub mod claude_code_step;
pub mod doc_materializer;
pub mod email;
pub mod fan_out;
pub mod harvest_approve;
pub mod harvest_gate;
pub mod http_post;
pub mod materialize_doc;
pub mod merge_contacts;
pub mod openai_compat_transport;
pub mod opportunity_edit;
pub mod suspend;

pub use aggregate::AggregateNode;
pub use channel_transport::{
    ChannelSendReceipt, ChannelTransport, OutboundAction, OutboundBody, StubChannelTransport,
    UnwiredChannelTransport, WorkflowTriggerDispatch,
};
pub use claude_code_step::{ClaudeCodeStep, MetaTransport, TransportInfo};
pub use doc_materializer::{
    doc_materializer_live, DocMaterializer, MaterializeDiagnostic, MaterializeOutcome,
    MaterializedFile, OpportunityEdit, RecordedEditCall, RecordedMaterializeCall,
    StubDocMaterializer,
};
pub use email::{EmailChannelTransport, DEFAULT_EMAIL_FROM, EMAIL_FROM_ENV, RESEND_API_KEY_ENV};
pub use fan_out::FanOutNode;
pub use harvest_approve::HarvestApproveNode;
pub use harvest_gate::{pending_harvest_record, HarvestDecision, HarvestGate, HarvestMode};
pub use http_post::{http_post_live, HttpPost, HttpPostResponse, ReqwestHttpPost, StubHttpPost};
pub use materialize_doc::MaterializeDocNode;
pub use merge_contacts::MergeContactsNode;
pub use openai_compat_transport::{
    default_local_http_post, openai_compat_meta_transport, openai_compat_meta_transport_live,
    openai_compat_transport, openai_compat_transport_live, LocalHttpPost,
};
pub use opportunity_edit::{OpportunityEditNode, OpportunityEditOp};
pub use suspend::{SuspendNode, DEFAULT_IDENTITY as SUSPEND_NODE_DEFAULT_IDENTITY};
