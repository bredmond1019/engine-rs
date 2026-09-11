---
type: Reference
title: Heavy-Work Queue (EN.17.I)
description: The FIFO heavy-work queue that admits Rust SDLC check runs by per-class limit and free memory, reclaims by liveness never by age, and what it does not gate
doc_id: heavy-work-queue
layer: [engine]
project: engine-rs
status: active
keywords: [heavy work queue, admission, fifo, reclaim, heartbeat, fleet build]
related: [architecture, engine-rs-testing]
created: 2026-09-11
updated: 2026-09-11
---

# Heavy-Work Queue (`EN.17.I`)

A generic FIFO queue in `engine-core`'s coordination layer that bounds how many heavy Rust check
runs — `SDLC_TASK`'s and `SDLC_FLOW`'s test stage, and `SDLC_FLOW`'s final validation — execute
concurrently on one host, per admission class, with a free-memory floor and reclaim driven by
liveness rather than elapsed time.

## Why this exists

Concurrent Rust lanes compiling at once is this fleet's measured contention failure: a load
average of 63 on a 10-core Mini, 15 concurrent `rustc` processes, and 2.7G free. The only gate
before this block, `scripts/fleet_build.py`, covered one repo and leaked by construction: its
`_sweep_stale` deletes any permit whose `started_at` is older than `FLEET_BUILD_TTL_SECONDS`
(300s) even when the holder pid is still alive, and `started_at` is never refreshed — so a live
build running longer than five minutes (the length of a real `cargo nextest run --workspace` on a
cold tree) loses its slot to the next acquirer. This queue fixes that by reclaiming on **liveness**
(holder pid alive, heartbeat fresh), never on start age.

## The job record and its store

`crates/engine-core/src/coord/heavy_work.rs` defines `HeavyWorkJob`, one record per admission
attempt, serialized `snake_case`:

```rust
pub struct HeavyWorkJob {
    pub job_id: Uuid,
    pub class: String,           // e.g. "test" / "build" -- any string a caller names
    pub state: JobState,         // Queued | Running | Done | Cancelled | Abandoned
    pub repo: String,
    pub cwd: PathBuf,
    pub commands: Vec<String>,
    pub run_id: Option<Uuid>,
    pub holder_pid: Option<u32>,
    pub enqueued_at: DateTime<Utc>,
    pub admitted_at: Option<DateTime<Utc>>,
    pub heartbeat_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub passed: Option<bool>,
}
```

Each job is one JSON file at `<lock_dir>/heavy-work/jobs/<job_id>.json` (`lock_dir` comes from the
existing `coord::resolve_lock_dir`), written temp-file-plus-rename so a reader never observes a
partial write. `Cancelled` is reserved for a future explicit-cancel caller; nothing in this block
writes it.

## Configuration: `brain.toml`'s `[heavy_work]` table

The queue reads a `[heavy_work]` table from the brain root's `brain.toml` — **not** from this
repo's `planning/harness.json`. See "Why brain.toml, not harness.json" below.

```toml
[heavy_work]
heartbeat_interval_secs = 30   # default when omitted
stale_after_secs = 300         # default when omitted -- mirrors fleet_build.py's TTL
poll_interval_ms = 50          # default when omitted -- mirrors fleet_build.py's poll cadence

[heavy_work.classes.test]
limit = 2
min_free_mb = 2048

[heavy_work.classes.build]
limit = 2
min_free_mb = 2048
```

- **No `[heavy_work]` table at all** parses to `HeavyWorkConfig::disabled()` — every check runs
  inline, unqueued, exactly as before this block landed (standing rule 6's behaviour-stable
  default).
- **A present table with a class `limit = 0`** fails loudly, naming the exact key
  (`heavy_work.classes.<name>.limit`) via `HeavyWorkConfigError::InvalidClassLimit`.
- **A job submitted with a class the table does not define** runs unqueued with a
  `tracing::warn!` and its outcome reports `degraded: true` — never refused.
- Classes are a map, so adding a new admission class (e.g. a browser-test class) is a `brain.toml`
  edit, not a code change.

This crate parses `[heavy_work]` with its own minimal struct rather than reusing
`mev::brain::config::BrainConfig` (which `policy::permission` already uses for
`[permission_profiles]`): that reader has no `heavy_work` field and no `deny_unknown_fields`, so
handing it this file would silently ignore the table instead of surfacing it. Every other
top-level `brain.toml` table (`[vocab]`, `[[repos]]`, `[permission_profiles]`, …) is simply absent
from this module's struct and ignored by `toml::from_str` — verified against `mev`'s config
2026-09-10.

## Admission: FIFO per class, gated on liveness count and free memory

`HeavyWorkQueue::submit(spec, work) -> JobHandle<T>` persists a `Queued` record and returns a
future; `HeavyWorkQueue::run(spec, work)` is `submit(..).await.await`. The queue's worker is a
tokio task started lazily once per lock dir behind a process-global `OnceLock` (mirroring
`journal::install_durable_handle`'s own process-global handle).

Admission is strictly ordered by `(enqueued_at, job_id)` per class — a blocked head is never
skipped past. A queued job admits only when **both** hold:

- the count of that class's **live** `Running` records — holder pid alive **and**
  `heartbeat_at` within `stale_after_secs` — counted under an flock on
  `<lock_dir>/heavy-work/.admission.lock`, is below `limit` (so a second process on the host
  shares the same limit);
- the `FreeMemoryProbe` reading is at or above `min_free_mb`, **re-read on every dequeue
  attempt** — never cached.

The default `FreeMemoryProbe` sums `vm_stat` free plus inactive pages (mirroring
`fleet_build.py`'s `_vm_stat_free_mb`) and returns `+inf` when `vm_stat` is unreadable or
unparseable, never an error. `work` itself runs on `tokio::task::spawn_blocking`, because the
`CommandRunner` it wraps is synchronous.

## Reclaim: liveness only, never start age

While a job runs, a heartbeat task restamps `heartbeat_at` every `heartbeat_interval_secs`. A
`Running` record is reclaimable (transitioned to `Abandoned`, its slot freed) **only** when:

- its holder pid is no longer running, **or**
- its `heartbeat_at` is older than `stale_after_secs`.

`admitted_at` and `enqueued_at` are never inputs to this decision — the reclaim classifier's own
signature takes only the pid, `heartbeat_at`, the threshold, and an injected clock. This is the
queue's central fix over `fleet_build.py`'s TTL sweep: a long-running, genuinely-alive job keeps
its slot indefinitely instead of being evicted at a fixed age. On worker start, this process's own
leftover `Queued`/`Running` records from a previous pid are marked `Abandoned` — their work
closures died with that process.

## The SDLC consumers

When the queue is enabled:

- `TestTaskNode::process` (`crates/engine-core/src/workflows/sdlc_flow/task_loop.rs`) wraps its
  `run_checks` call in `HeavyWorkQueue::run` with class `test`. Used by both `SDLC_TASK` and
  `SDLC_FLOW`'s per-task test stage.
- `FinalValidationNode::process` (`crates/engine-core/src/workflows/sdlc_flow/final_validation.rs`)
  wraps its checks with class `build`.

Both nodes stamp `heavy_work: { mode, job_id, class, waited_ms, degraded }` into their `ctx.nodes`
output **at every setting** — `mode: "disabled"` with null fields when the queue is off, so the
output shape never varies between an enabled and a disabled run. When the queue is enabled but the
lock dir is unwritable, the node still runs every selected check and stamps
`heavy_work.degraded == true` rather than failing the task.

An admitted job's own subprocess calls run through
`workflows::admitted_command_runner(SpecCommandRunner) -> CommandRunner`
(`crates/engine-core/src/workflows/mod.rs`), which adds `FLEET_BUILD_PREADMITTED=1` to the
command's env alongside whatever the caller already set. A stub runner injected with
`TestTaskNode::with_runner` continues to be used unchanged — the seam wraps, it does not replace.

## The `FLEET_BUILD_PREADMITTED` hand-off

`scripts/fleet_build.py` still fronts every `test`/`build` harness command engine-rs's own
`planning/harness.json` runs — that wrapper is unchanged for a JS-driven engine-rs run. When
`FLEET_BUILD_PREADMITTED` is set to any truthy value, `fleet_build.py`'s `main` skips permit
acquisition and `_sweep_stale` entirely and runs the wrapped command directly, passing through its
real exit code with no extra output of its own. This is the contract between a job already
admitted by `HeavyWorkQueue` (a Rust-driven run) and the Python wrapper it would otherwise also go
through: gated once, by the queue, not twice.

`_sweep_stale` itself is unchanged by this block — its TTL-vs-liveness defect
(`fleet-build-ttl-sweeps-a-live-permit`, filed as a repo carryover) still affects any invocation
that is **not** preadmitted: a JS-driven engine-rs run, or a human running `fleet_build.py`
directly.

## Status view: `GET /api/coordination/heavy-work`

`engine-serve`'s `http.rs` registers this route beside the other `/api/coordination/*` routes,
with **no `X-API-Key` gate** (matching its siblings). It resolves the brain root the same way
`get_coordination` does and returns, always `200`:

```json
{
  "enabled": true,
  "free_mb": 5120,
  "classes": [
    { "name": "build", "limit": 2, "min_free_mb": 2048, "running": 1, "queued": 0 },
    { "name": "test", "limit": 2, "min_free_mb": 2048, "running": 0, "queued": 1 }
  ],
  "jobs": [ /* HeavyWorkJob, newest first */ ]
}
```

Every still-active (`Queued`/`Running`) job is always included; only the terminal states
(`Done`/`Cancelled`/`Abandoned`) are capped at the 50 most recent, so the cap never hides work
currently in flight. A disabled queue, an unreadable `brain.toml`, or a missing jobs directory all
read as `enabled: false` / empty arrays — a normal, useful answer, not a fault. This is a **new**
route with its own JSON shape, not a new field on `CoordinationView` — bastion's `coord_cli.rs`
tests construct that struct as a literal, so adding a field there would break bastion's build.

## Why `brain.toml`, not `planning/harness.json`

A queue limit is a shared per-host resource, not a per-run policy knob. Standing rule 6's
four-layer policy resolution (event override > profile > harness.json > default) would let one
run's override change the cap every other concurrent run is admitted under — a limit only means
something if every consumer reads the same value. Child SDLC runs also resolve policy from their
own repo worktree's `harness.json` (`PolicyConfigSource::Worktree(invocation.repo_path)`), so an
HQ-scoped harness.json key would never even be seen by the test stage it is meant to gate.
`brain.toml` is the fleet's shared registry and machine config (`[attention]`,
`[permission_profiles]`), and `engine-core` already resolves it through `brain_root`. This repo's
own `planning/harness.json` carries only a `heavy_work` `_comment` pointer to this doc, mirroring
the `email_adapter` precedent for non-knob configuration.

## What this queue does not gate

- **JS engines** (`sdlc-flow.js` / `sdlc-task.js`) — unchanged. They keep today's lane caps and,
  in this repo, the `fleet_build.py` wrapper as before.
- **Parking a workflow while its job waits, and resuming it on completion** — `EN.17.J`. Here the
  wait is an awaiting future inside the node; there is no node-boundary pause/resume yet.
- **`fleet_build.py`'s TTL sweep of a live permit** — after this block it affects only JS-driven
  engine-rs runs and humans invoking the script directly.
- **`fleet_concurrency_check.py`'s native-build lane cap of 4** — remains stacked on this queue,
  unreconciled; that is a base-template decision.
- **Per-host limits** — `brain.toml` is shared by every host through git, so the same numbers
  apply everywhere.
- **Bounding how long a job may wait in the queue.**
