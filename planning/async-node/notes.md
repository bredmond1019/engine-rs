---
type: Note
title: Async Node Execution — Should Node::process Become async fn?
description: Whether to make engine-core's Node trait async before EN.2.A (ClaudeCodeStep) locks in a sync execution model, with a full file/struct/function map of both the Rust and Python execution cores for comparison.
doc_id: async-node
layer: [engine]
project: engine-rs
status: draft
keywords: [async, tokio, Node trait, ParallelNode, ClaudeCodeStep, EN.2.A, concurrency]
related: [master-plan, architecture, context]
---

# Async Node Execution — Should `Node::process` Become `async fn`?

> **Status:** draft — pre-plan holding area.
> **Promote with:** `/plan "async node execution"` · `/chore "async node execution"`
> **Backlog entry:** `agentic-portfolio/planning/backlog.md`

## What & Why

`engine-rs`'s node/workflow execution core (`engine-core`) is currently fully **synchronous** —
ported faithfully from the Python `orchestrator`, which is *also* fully synchronous at the
node/workflow level (confirmed by direct read, see below — this was a surprising finding,
not an assumption). Async only exists today at the infrastructure edges: `actix-web` HTTP
handlers, `sqlx` Postgres I/O, and the durable-write background task.

This matters right now because **EN.2.A (Claude Code step node) is paused** on a transport
decision (see `planning/handoff.md` and `planning/state.json` `carryover[]`:
`transport-decision-uses-d4-not-d3`, `claude-sdk-rs-not-on-disk`). Spawning a Claude Code
session is inherently an async operation (process spawn + await completion). If `Node::process`
stays synchronous, `ClaudeCodeStep` will have to block a whole OS thread for the entire session
duration (via `web::block`/`spawn_blocking`) — the same ceiling Python has, no leverage gained
from Rust's async runtime. If `Node::process` becomes `async fn` now, before EN.2.A's spec is
written, concurrent runs and I/O-bound branches could share the Tokio runtime instead of each
claiming a dedicated thread — the actual "Rust + async" advantage Python is structurally stuck
without (rewriting `pydantic_ai`'s sync integration would be a large lift on their side).

**This is an architectural decision that needs to be made explicitly, not left to fall out
as a side effect of the EN.2.A transport choice** — changing the `Node` trait's signature
ripples through everything already built in Phase 1 (`Router`, `ParallelNode`,
`WorkflowValidator`, the `on_progress` seam).

## Context & Background

- Governing decision: [D42](../../../../docs/decisions/D42-rust-engine-parallel-pilot.md) —
  `engine-rs` is a parallel-pilot rewrite of the Python `orchestrator`, graduating per-workflow,
  not big-bang. Python stays the production path until parity.
- Phase 1 (`EN.1.A`/`EN.1.B`/`EN.1.C`) is **Done** and merged to `main` — the `Node` trait,
  `Workflow::run`, `Router`, `ParallelNode`, `WorkflowValidator`, and the `bastion serve` HTTP
  embedding all exist and are tested. All of it is synchronous at the node level today.
- `EN.2.A` (Claude Code step node) is the very next block and is **paused** pending: (1) the
  user reviewing an external Claude Code Rust SDK on GitHub against the existing `claude-sdk-rs`,
  and (2) the transport decision that review informs. See `planning/handoff.md` for full
  detail on that pause.
- Master-plan section for `EN.2.A` (`planning/master-plan.md`, "Phase 2 — Claude Code step +
  control loop") frames the launcher (`claude-sdk-rs::execute_claude` + `Config`) vs. the
  tmux/file-drop `bastion ask` seam as the open transport question — it does **not** currently
  raise the sync-vs-async `Node` trait question at all. That gap is what this note captures.

## Key Information / Instructions

### Rust side (`engine-rs`) — current state, verified by direct grep/read this session

All node/workflow execution is synchronous. Relevant symbols:

- **`crates/engine-core/src/node.rs:49`** — the `Node` trait's core method:
  ```rust
  fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError>;
  ```
  Plain sync `fn`, not `async fn`. Every node implementation across the workspace overrides
  this signature (e.g. `crates/engine-core/src/workflow.rs:212`, `:226`;
  `crates/engine-serve/src/http.rs:157`; test fixtures in `dispatch.rs`, `parallel.rs`).
- **`crates/engine-core/src/workflow.rs`** — `Workflow::run` is the pointer-walk runner:
  synchronous `while current_node { … }` loop, no `.await` anywhere in the walk. Calls
  `Node::process` directly, then resolves the next node via `Router::route(ctx)` (for router
  nodes) or `NodeConfig::next()`/`connections[0]` (for plain nodes).
  - `OnProgress<'a>` type alias — `Box<dyn FnMut(&TaskContext) + 'a>` — the injected
    persistence seam, itself synchronous (called inline in the walk).
  - `Workflow::new_validated(registry, schema)` — fallible constructor, runs
    `WorkflowValidator::validate` first.
- **`crates/engine-core/src/parallel.rs:53`** — `ParallelNode::process` fans out over
  **`std::thread::scope`** (real OS threads), not async tasks:
  ```rust
  let branch_results: Vec<Result<TaskContext, NodeError>> = std::thread::scope(|scope| { … });
  ```
  Deep-copies `TaskContext` per branch, deterministic last-write-wins merge of `nodes`/
  `node_runs` on key collision.
- **`crates/engine-core/src/routing.rs`** — `Router` trait (supertrait of `Node`):
  `fn route(&self, ctx: &TaskContext) -> Option<String>` — also synchronous.
- **`crates/engine-core/src/validate.rs`** — `WorkflowValidator`/`ValidationError` — static
  graph-shape checks (BFS reachability, DFS cycle detection skipping router edges, fan-out
  arity guard). Not concurrency-related directly, but any `Node` trait signature change needs
  to keep flowing through this (router classification is via `NodeRegistry` lookup +
  `Node::as_router().is_some()`).
- **`crates/engine-serve/src/http.rs`** — where sync execution meets the async runtime:
  - `async fn post_events(...)` (line ~93) is the `POST /events/` handler — genuinely async
    (actix-web).
  - It wraps the **synchronous** `workflow.run(...)` call inside **`web::block(move || { … })`**
    (line ~123) — Actix's blocking-thread-pool escape hatch. This is the current workaround:
    the sync workflow execution doesn't block the async reactor thread, but it still occupies
    a full OS thread from Tokio's blocking pool for the run's entire duration.
  - Comment at `http.rs:120-122` explains why the `OnProgress` box is built *inside* the
    blocking closure: the trait object carries no `Send` bound of its own and can't cross the
    `web::block` boundary as a value otherwise.
- **`crates/engine-serve/src/durable.rs`** — the one place genuine `async`/Tokio concurrency
  already exists in this codebase:
  - `DurableHandle`, `spawn_durable_writer(pool: Option<PgPool>) -> DurableHandle` — spawns a
    real `tokio::spawn` background task draining an `mpsc::UnboundedReceiver<DurableMessage>`,
    `.await`-ing `engine_store::insert_event`/`update_event` calls against `sqlx::PgPool`.
  - `durable_on_progress(handle, run_id, workflow_type, data) -> impl FnMut(&TaskContext) + Send + 'static`
    — bridges the sync `on_progress` seam to this async writer via channel send (non-blocking).
- **`crates/engine-serve/src/live_state.rs`** — `LiveStateStore` —
  `Arc<RwLock<HashMap<RunId, TaskContext>>>` — sync `record`/`get`/`list_active`/`remove`, no
  async needed (in-memory map).
- **`crates/engine-store/src/postgres.rs`** — `sqlx::PgPool` connect + `insert_event`/
  `update_event`/`get_event` — genuinely async (tokio + sqlx runtime, per
  `planning/decisions/D2-async-runtime-choice.md`).

### Python side (`orchestrator`) — current state, verified by direct read this session

Also fully synchronous at the node/workflow level — this was the surprising finding that
prompted this note. Relevant symbols:

- **`app/core/workflow.py`** — `Workflow.run`:
  ```python
  def run(
      self,
      event: Any,
      on_progress: Callable[[TaskContext], None] | None = None,
  ) -> TaskContext:
  ```
  Plain sync `def`. Core loop:
  ```python
  while current_node_class:
      current_node = self.nodes[current_node_class].node
      with self.node_context(current_node_class.__name__, task_context):
          task_context = current_node().process(task_context)
      if on_progress:
          on_progress(task_context)
      current_node_class = self._get_next_node_class(current_node_class, task_context)
  ```
  No `await` anywhere in the walk.
- **`app/core/nodes/base.py`** — `Node.process` abstract method:
  ```python
  @abstractmethod
  def process(self, task_context: TaskContext) -> TaskContext:
  ```
  Plain sync abstract method, not `async def`.
- **`app/core/nodes/agent.py`** — `AgentNode` (wraps `pydantic_ai.Agent`, 8 `ModelProvider`s
  incl. openai/anthropic/gemini/ollama/bedrock/`claude_code_sdk`/`claude_code_session`).
  `process` is sync; the LLM call in `run_agent_recorded` is:
  ```python
  result = self.agent.run_sync(user_prompt=user_prompt)
  ```
  `run_sync` — pydantic_ai's blocking wrapper. Even though `pydantic_ai`/`httpx` construct
  async clients (`AsyncClient`, `AsyncAzureOpenAI`) under the hood, the node layer blocks the
  calling thread per LLM call. No node-level async concurrency.
- **`app/core/nodes/parallel.py`** — `ParallelNode` fan-out uses
  **`concurrent.futures.ThreadPoolExecutor`**, not `asyncio.gather`/`TaskGroup`:
  ```python
  with ThreadPoolExecutor() as executor:
      for node in node_config.parallel_nodes:
          cloned_context = task_context.model_copy(deep=True)
          future = executor.submit(node().process, cloned_context)
          future_list.append(future)
      results = [future.result() for future in future_list]
  ```
  Thread-based, same model `ParallelNode`'s Rust port mirrors via `std::thread::scope`.
- **`app/core/nodes/router.py`** — `BaseRouter`/`RouterNode` — branching with fallback,
  ported to Rust's `Router` trait + `dispatch_route()`.
- **`app/core/nodes/tool_use.py`** — `ToolUseNode` — raw Anthropic tool-use loop, bounded
  `max_iterations` (default 10). **No Rust equivalent exists yet** — not part of Phase 1/2 scope
  as currently planned.
- **`app/core/validate.py`** (151 LOC) — graph validator Rust's `WorkflowValidator` ports from
  — already existed in Python; not a Rust-only addition.
- **`app/worker/tasks.py`** — `process_incoming_event`, the Celery task:
  ```python
  @celery_app.task(name="process_incoming_event")
  def process_incoming_event(event_id: str):
  ```
  Plain sync Celery task — no `async def`, no `asyncio.run(...)` bridge. Opens a sync DB
  session, loads the event, calls `workflow.run(...)` synchronously inline in the worker
  process.
- **`app/api/endpoint.py`** (or wherever the events route lives) — `POST /events/` FastAPI
  route:
  ```python
  @router.post("/", status_code=HTTPStatus.ACCEPTED, dependencies=[Depends(require_api_key)])
  def handle_event(payload: EventPayload, session: Session = Depends(db_session)) -> Response:
  ```
  Also plain sync `def`, not `async def`. Does sync DB writes, then
  `celery_app.send_task("process_incoming_event", args=[str(event.id)])` — a synchronous,
  non-blocking hand-off (publishes to the broker) — returns 202 immediately.
- **`app/worker/config.py`** — Celery + Redis broker/result backend, JSON serialization —
  this is where Python's actual concurrency comes from: multiple workflow *runs* execute in
  parallel because they're on separate OS **processes** (Celery workers), not because any
  single run is internally async.
- **`app/core/` total size**: 1,212 LOC (task.py 139, validate.py 151, workflow.py 216,
  schema.py 71, commands/init_workflow.py 114, nodes/: agent.py 205, tool_use.py 128,
  router.py 77, base.py 58, parallel.py 48, `__init__.py` 5) — confirms the master-plan's
  "~1,100 LOC" figure is roughly right (slightly under actual).

### Side-by-side concurrency model

| Concern | Python `orchestrator` | Rust `engine-rs` (today) |
|---|---|---|
| Node execution | sync `def process` | sync `fn process` |
| Workflow walk | sync `while` loop, no await | sync `while` loop, no await |
| Parallel branch fan-out | `ThreadPoolExecutor` (threads) | `std::thread::scope` (threads) |
| Whole-run concurrency (multiple triggers) | Celery workers — separate OS **processes** | `web::block` — Tokio blocking **thread pool**, single process |
| HTTP request handler | sync `def` FastAPI route | `async fn` actix-web handler |
| DB writes (durable record) | sync `Session` | async `sqlx::PgPool` via background `tokio::spawn` task |
| LLM/agent call | `agent.run_sync(...)` — blocking | N/A yet — no `AgentNode`/`ClaudeCodeStep` equivalent built |

**Conclusion from this comparison:** the current fully-synchronous Rust node/workflow core is
a *faithful port*, not a regression — Python never had real `asyncio` concurrency at this layer
either. The opportunity is that Rust *could* exceed Python's model by making `Node::process`
genuinely `async fn`, which Python can't easily retrofit (would require reworking
`pydantic_ai`'s sync integration). Whether to take that opportunity now, before `EN.2.A` locks
in a signature, is the open question below.

## Open Questions

- Should `Node::process` become `async fn` (e.g. via native RPITIT or the `async_trait` crate)
  before `EN.2.A`'s spec is written? This changes the trait every existing node implementation
  overrides, and ripples into `Router::route`, `ParallelNode`'s fan-out mechanism, and the
  `OnProgress` seam signature.
- If `Node::process` becomes async, does `ParallelNode`'s fan-out change from
  `std::thread::scope` to `tokio::spawn` + join (lighter-weight for I/O-bound branches, e.g.
  branches that each call out to Claude Code), or stay thread-based for CPU-bound work? Could
  become a per-branch choice rather than one-size-fits-all.
- Does an async `Node::process` retire the `web::block` wrapper in
  `crates/engine-serve/src/http.rs:post_events`, or does something still need `spawn_blocking`
  for genuinely CPU-bound nodes?
- Is this its own decision/block (worth a `planning/decisions/D5-*.md`, since D3 and D4 are
  already spoken for per the `carryover[]` constraint), or does it get folded directly into
  `EN.2.A`'s scope once `/generate-tasks EN.2.A` resumes?
- Does the whole-run concurrency model (currently `web::block`'s bounded thread pool, one
  process) need to eventually match Celery's ability to scale out across multiple machines —
  or is single-process-multi-thread sufficient for Bastion's actual load (a solo practice OS,
  not a multi-tenant service)?

## Rough Scope

Not sized yet — this note exists to make the decision well-informed before `/generate-tasks
EN.2.A` resumes (see `planning/handoff.md`), not to scope an implementation. If the answer is
"yes, make `Node::process` async," the touched surface is: `engine-core/src/node.rs` (trait
signature), `engine-core/src/workflow.rs` (the pointer-walk loop + `OnProgress` seam),
`engine-core/src/routing.rs` (`Router::route`), `engine-core/src/parallel.rs` (fan-out
mechanism), every existing `Node` impl (test fixtures across `dispatch.rs`, `parallel.rs`,
`http.rs`, `workflow.rs`), and `engine-serve/src/http.rs` (`post_events`'s `web::block` wrapper).
