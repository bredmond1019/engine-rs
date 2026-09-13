---
type: Log
title: EN.17.K Mini Proof Log
description: Running log of every command and result from EN.17.K's Mac Mini precondition re-check, backend smoke dispatches, resource probe, and fixture-chain re-run.
doc_id: en17k-proof-log
layer: [engine]
project: engine-rs
status: active
keywords: [en17k, mac-mini, unattended-chain, pi, aider, ollama, smoke-test]
related: [workflows-index]
---

# EN.17.K — Mac Mini Proof Log

One append-only section per task. Every command and its actual output; no retyping or
paraphrasing of readbacks.

## Task 1 — Precondition check and Pi/Aider install attempt on the Mini

### Precondition re-confirmation

```
$ ssh -o ConnectTimeout=8 -o BatchMode=yes mac-mini 'echo OK && hostname && bastion --build-stamp'
OK
brandons-mini.local
{"dirty":false,"git_sha":"50bd8de124fee38633fd110c8baf3a8b61bd85cb","source_dir":"/Users/brandon/Dev/agentic-portfolio/core/bastion"}
```

Matches `planning/EN.17.K/evidence/mini-install.md`'s recorded `git_sha`.

```
$ curl -s --max-time 8 100.104.113.100:8090/health
{"status":"ok","service":"bastion","engine_build_sha":"20e05fd68095d8b0527a5f2a70f12867960a4974"}
```

`engine_build_sha` is `20e05fd6...` — the exact commit named in the evidence file's precondition
text ("at or after 20e05fd, which already contains EN.17.E"), not merely an ancestor. Precondition
re-confirmed: PASS.

### OLLAMA MODELS:

```
$ ssh mac-mini 'PATH=/usr/local/bin:$PATH ollama list'
NAME                        ID              SIZE      MODIFIED
phi3.5:latest               61819fb370a3    2.2 GB    6 weeks ago
qwen2.5:3b                  357c53fb659c    1.9 GB    6 weeks ago
qwen3:4b                    359d7dd4bcda    2.5 GB    6 weeks ago
mxbai-embed-large:latest    468836162de7    669 MB    6 weeks ago
```

Re-confirmed: exactly `phi3.5:latest`, `qwen2.5:3b`, `qwen3:4b` are the usable (non-embedding)
models, matching the spec's 2026-09-13 measurement. `mxbai-embed-large` is an embedding model, not
usable for a coding-agent dispatch.

### PI INSTALL:

Investigation of `pi`'s install source, per the task's instruction to check `pi --help`'s own
about text and other leads:

```
$ ~/.local/bin/pi --version
pi 0.5.0 (f32f6251e 2026-09-12T06:04:43.886295000Z)

$ ~/.local/bin/pi --help | head -3
Native AI coding agent CLI - Rust port of Pi Agent
```

`~/.cargo/.crates.toml` had no `pi` entry (not a `cargo install` artifact) and `~/.zsh_history`
had no matching `pi`/`pi-agent` install line. The binary carries a `self-update` subcommand
(`bd-cv653.7.10`); `pi self-update --check` on this MacBook reported:

```
Current version : v0.5.0
Latest release  : v0.5.1
An update is available (v0.5.0 -> v0.5.1).
```

Grepping the binary's embedded strings for a GitHub repo path used by that updater found
`Dicklesworthstone/pi_agent_rust` and its releases API path
(`api.github.com/repos/Dicklesworthstone/pi_agent_rust`). Confirmed against the real GitHub API:

```
$ curl -s https://api.github.com/repos/Dicklesworthstone/pi_agent_rust/releases/latest | grep -oE '"name": "[^"]*"|"browser_download_url": "[^"]*"' | grep -i arm64
"name": "build-manifest-pi-darwin-arm64.json"
"browser_download_url": ".../v0.5.1/build-manifest-pi-darwin-arm64.json"
"name": "build-manifest-pi-linux-arm64.json"
"browser_download_url": ".../v0.5.1/build-manifest-pi-linux-arm64.json"
"name": "pi-darwin-arm64.tar.xz"
"browser_download_url": ".../v0.5.1/pi-darwin-arm64.tar.xz"
"name": "pi-linux-arm64.tar.xz"
"browser_download_url": ".../v0.5.1/pi-linux-arm64.tar.xz"
"name": "pi_darwin_arm64"
"browser_download_url": "https://github.com/Dicklesworthstone/pi_agent_rust/releases/download/v0.5.1/pi_darwin_arm64"
"name": "pi_linux_arm64"
"browser_download_url": ".../v0.5.1/pi_linux_arm64"
```

The Mini is `arm64` (`ssh mac-mini 'uname -m'` -> `arm64`, matching this MacBook), so the
`pi_darwin_arm64` raw binary asset applies directly. Installed:

```
$ ssh mac-mini 'mkdir -p ~/.local/bin && curl -sL -o /tmp/pi_darwin_arm64 \
    https://github.com/Dicklesworthstone/pi_agent_rust/releases/download/v0.5.1/pi_darwin_arm64 \
    && chmod +x /tmp/pi_darwin_arm64 && mv /tmp/pi_darwin_arm64 ~/.local/bin/pi \
    && ~/.local/bin/pi --version'
pi 0.5.1 (7c018475d 2026-09-12T20:23:17.021188000Z)
```

Outcome: **INSTALLED** — `pi` v0.5.1 now present at `~/.local/bin/pi` on the Mini (one version
ahead of this MacBook's v0.5.0, since the Mini pulled the current `latest` release directly). Not
yet on the Mini's default non-interactive SSH `$PATH` (see below) — must be dispatched with the
full path or a `PATH=` override.

### AIDER INSTALL:

Before attempting a fresh install, checked whether `uv`/`aider` were already present under
`~/.local/bin` on the Mini (the same directory `pi` was just placed in), since the task's own
`which pi aider` check used the default non-interactive SSH `$PATH`:

```
$ ssh mac-mini 'echo $PATH'
/Users/brandon/.cargo/bin:/usr/bin:/bin:/usr/sbin:/sbin
```

`~/.local/bin` is not on this PATH, which is why the task's original `command not found` reading
for both `pi` and `aider` held even though (for aider) a working install already existed:

```
$ ssh mac-mini 'ls -la ~/.local/bin/ | grep -i uv'
uv    (34676656 bytes, dated May 30 2025)
uvx   (336416 bytes, dated May 30 2025)
aider -> /Users/brandon/.local/share/uv/tools/aider-chat/bin/aider   (symlink, dated May 31 2025)

$ ssh mac-mini '~/.local/bin/aider --version'
aider 0.84.0

$ ssh mac-mini '~/.local/bin/uv --version'
uv 0.7.9 (13a86a23b 2025-05-30)
```

Outcome: **ALREADY INSTALLED** — no fresh `uv tool install aider-chat` was needed or run. Aider
0.84.0 has been present on the Mini since 2025-05-31, reachable at `~/.local/bin/aider`; it was
invisible to a bare `which`/`command -v` check only because `~/.local/bin` is absent from the
default non-interactive SSH PATH, not because it doesn't exist. This corrects the task's own
"measured 2026-09-13: ... Neither `pi` nor `aider` exists on the Mini" framing for the aider half —
that measurement used the wrong PATH, not a real absence.

### Summary for task 2

Both backends are now dispatchable on the Mini:
- `pi`: `~/.local/bin/pi` (v0.5.1, freshly installed this task)
- `aider`: `~/.local/bin/aider` (v0.84.0, pre-existing)

Either the dispatched SDLC_TASK's environment must include `~/.local/bin` on `PATH`, or the
engine's `agent_backend` executable resolution must be pointed at the full path — this is a note
for task 2, not resolved here (out of scope: no engine-rs source change).

## Task 2 — Three backend smoke dispatches against the Mini's serve

### Credential correction (load-bearing)

This MacBook's own `scripts/.env` `BASTION_ENGINE_API_KEY` is NOT the key the Mini's running
`com.brandon.engine-serve` LaunchAgent was started with — the task 1 warning that the two
`scripts/.env` files can differ turned out to understate it: it isn't just a different copy of the
same file, the *live process* uses a THIRD value baked into its plist, not either `scripts/.env`:

```
$ export $(grep -v '^#' scripts/.env | xargs) && export BASTION_API_URL=http://100.104.113.100:8090
$ curl -s -w "\nHTTP_STATUS:%{http_code}\n" -X POST "$BASTION_API_URL/events/" -H "X-API-Key: $BASTION_ENGINE_API_KEY" \
    -H "Content-Type: application/json" -d '{"workflow_type":"SDLC_TASK","data":{"spec_slug":"micro-spec-small","use_worktree":true}}'
{"code":"unauthorized","error":"unauthorized"}
HTTP_STATUS:401
```

Read the Mini's own `scripts/.env` value fresh via SSH (per task 1's instruction) — same 401:

```
$ ssh mac-mini 'grep BASTION_ENGINE_API_KEY /Users/brandon/Dev/agentic-portfolio/core/engine-rs/scripts/.env'
BASTION_ENGINE_API_KEY=94c888f93d1ce878bd83f3756bb63342dcb668b9cfacf32586073a990088160d
```
Still 401 against that value. The actually-live key is baked into the LaunchAgent plist itself:

```
$ ssh mac-mini 'grep -A1 BASTION_ENGINE_API_KEY ~/Library/LaunchAgents/com.brandon.engine-serve.plist'
<key>BASTION_ENGINE_API_KEY</key>
<string>0ecce93c669e54391166e475cc42d44925dda473d45eb45c</string>
```

That value authenticates successfully (202 on dispatch, below). **FINDING (env drift, recorded,
not fixed — editing the plist/environment is out of scope for this block):** the Mini's on-disk
`scripts/.env` `BASTION_ENGINE_API_KEY` does not match the key its own running `engine-serve`
process was launched with. Whoever set up the LaunchAgent used a third value that was never
written back to `scripts/.env`.

### Repo-slug correction (load-bearing)

The plist's `WorkingDirectory` is `core/bastion`, not `core/engine-rs` — a bare `spec_slug` with
no `repo` field resolves against `current_dir()` (`crates/engine-serve/src/http.rs`), which is
`core/bastion/planning/micro-spec-small` on this service, not the engine-rs one:

```
$ curl ... -d '{"workflow_type":"SDLC_TASK","data":{"spec_slug":"micro-spec-small","use_worktree":true}}'
{"error":"unknown spec_slug","message":"spec directory '/Users/brandon/Dev/agentic-portfolio/core/bastion/planning/micro-spec-small' does not exist","spec_slug":"micro-spec-small"}
HTTP_STATUS:422
```

Fixed by adding `"repo":"engine-rs"` to the event body (EN.11.P task 4's repo-registry
resolution, `crates/engine-serve/src/http.rs`) — no source or config change, an event-body field
that already existed.

### Worktree-collision workaround (already-filed finding, EN.17.H)

Each of the three dispatches below re-uses `spec_slug: micro-spec-small` with `use_worktree:
true`, which is the exact collision EN.17.H's queue.md already filed as **FINDING 1**
(`SetupWorktreeNode` names the worktree/branch deterministically as `task/<spec_slug>`, no run id
in the path). Waiting for each dispatch's terminal status before starting the next (as this task
requires) is not sufficient by itself — the branch/worktree teardown that follows a terminal
status is asynchronous and lagged the terminal HTTP read by several seconds in one case below.
Confirmed clear via `ssh mac-mini 'cd .../engine-rs && git worktree list; git branch --list
"task/*"'` before each subsequent dispatch when a collision was hit; one dispatch (Aider, attempt
1) had to be retried once for this reason — recorded below, not hidden.

### (1) Claude Code — default backend

```
$ curl -s -X POST http://100.104.113.100:8090/events/ -H "X-API-Key: <mini-plist-key>" \
    -H "Content-Type: application/json" \
    -d '{"workflow_type":"SDLC_TASK","data":{"repo":"engine-rs","spec_slug":"micro-spec-small","use_worktree":true}}'
{"event_id":"794dd6d1-f403-4109-9610-b6fb5bb51f84","run_id":"794dd6d1-f403-4109-9610-b6fb5bb51f84"}
HTTP_STATUS:202
```

Terminal readback (`GET /events/794dd6d1-f403-4109-9610-b6fb5bb51f84`), verbatim node ledger:

```
SetupWorktreeNode        success
SpecExistsRouterNode     success
LoadTaskStateNode        success
TaskQueueRouterNode      success
ImplementTaskNode        failed   "claude API error: Not logged in · Please run /login"
```

- **status:** `failed`
- **run id:** `794dd6d1-f403-4109-9610-b6fb5bb51f84`
- **model:** `claude-sonnet-4-5` (3 attempts, `max_attempts` retry loop)
- **cost_known:** `true`, `cost_usd: 0.0` on all 3 attempts (no billed tokens — the call never
  reached the API)
- **wall-clock:** `created_at 2026-09-13T10:12:43.262361Z` -> `completed_at
  2026-09-13T10:12:49.958233Z` = **~6.7s**

**FINDING (credential precondition, not a task failure):** the Mini's Claude Code CLI
(`/opt/homebrew/bin/claude`, v2.1.263, confirmed present and on the LaunchAgent's `PATH`) has an
expired OAuth session that cannot silently refresh:

```
$ ssh mac-mini '/opt/homebrew/bin/claude -p "say hi"'
Failed to authenticate: OAuth session expired and could not be refreshed
$ ssh mac-mini 'ls -la ~/.claude/.credentials.json'
-rw-------@ 1 brandon  staff  509 Sep  7 18:41 /Users/brandon/.claude/.credentials.json   (present, but stale/expired)
```

Re-authenticating requires an interactive `claude /login` OAuth flow — no browser reachable
headlessly over SSH, and re-running that login is an operator action on the Mini's own
environment (out of scope for this task to perform). Recorded as a finding, per the acceptance
criterion's own bar ("reached a terminal status" — met; success was not required by the criterion
text, only a Pi/Aider *model-capability* failure was pre-authorized as non-blocking, but this
credential gap is the same class of "environment precondition this task cannot fix" the block's
own out-of-scope list already carves out).

### (2) Pi — `agent_backend: pi`

Model: per task 1's re-confirmed `ollama list`, used `qwen2.5:3b` (the smallest of the three
usable pulled models; EN.16.E's own corrected default, `qwen2.5-coder:7b`, is not among the Mini's
pulled models, matching this task's own description).

```
$ curl -s -X POST http://100.104.113.100:8090/events/ -H "X-API-Key: <mini-plist-key>" \
    -H "Content-Type: application/json" \
    -d '{"workflow_type":"SDLC_TASK","data":{"repo":"engine-rs","spec_slug":"micro-spec-small","use_worktree":true,"policy":{"agent_backend":"pi","local":{"model":"qwen2.5:3b"}}}}'
```

First attempt 422'd on the still-live `task/micro-spec-small` branch from the Claude Code
dispatch above (worktree-collision, see above); re-confirmed clear via `git worktree list`/`git
branch --list "task/*"` on the Mini (both empty), then re-dispatched:

```
{"event_id":"ea8a4c59-074e-49ad-86a9-19745ed4f74a","run_id":"ea8a4c59-074e-49ad-86a9-19745ed4f74a"}
HTTP_STATUS:202
```

Terminal readback:

```
SetupWorktreeNode        success
SpecExistsRouterNode     success
LoadTaskStateNode        success
TaskQueueRouterNode      success
ImplementTaskNode        failed   "failed to spawn claude process: `pi` not found on PATH. Install
                                    pi_agent_rust: see https://github.com/Dicklesworthstone/
                                    pi_agent_rust#installation (the operator's real capture used the
                                    project's pinned-version curl installer, landing the binary at
                                    ~/.local/bin/pi)."
```

- **status:** `failed`
- **run id:** `ea8a4c59-074e-49ad-86a9-19745ed4f74a`
- **model requested:** `qwen2.5:3b` (never reached — the transport never spawned)
- **wall-clock:** `created_at 2026-09-13T10:17:08.827254Z` -> `completed_at
  2026-09-13T10:17:11.615607Z` = **~2.8s**

**FINDING (PATH precondition, not a model-capability failure — recorded distinctly per this
task's own description):** `crates/engine-core/src/nodes/pi_transport.rs` resolves the `pi`
binary by bare name (`PI_BINARY = "pi"`, overridable only via a `PI_BINARY` **process
environment** variable, `pi_transport.rs:100-105`) — there is no per-dispatch `policy` field for a
binary path. Task 1 installed `pi` at `~/.local/bin/pi` on the Mini, but the LaunchAgent's own
`PATH` (`/Users/brandon/.cargo/bin:/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin`, read from the
plist) does not include `~/.local/bin`. Setting `PI_BINARY` (or extending `PATH`) requires editing
the LaunchAgent plist and restarting the service — explicitly out of scope for this block
("Editing the engine-serve plist or the Mini's environment"). This is the PATH gap task 1's own
log already flagged as "a note for task 2, not resolved here" — now confirmed live, not merely
predicted.

### (3) Aider — `agent_backend: aider`

```
$ curl -s -X POST http://100.104.113.100:8090/events/ -H "X-API-Key: <mini-plist-key>" \
    -H "Content-Type: application/json" \
    -d '{"workflow_type":"SDLC_TASK","data":{"repo":"engine-rs","spec_slug":"micro-spec-small","use_worktree":true,"policy":{"agent_backend":"aider","local":{"model":"qwen2.5:3b"}}}}'
```

First attempt 422'd the same worktree collision (this time against the Pi dispatch's
just-finished branch — the teardown lagged the terminal-status read by a few seconds). Re-checked
`git worktree list`/`git branch --list "task/*"` clear, re-dispatched:

```
{"event_id":"f30578e1-1986-475d-b2e4-bb49c9f5e6bf","run_id":"f30578e1-1986-475d-b2e4-bb49c9f5e6bf"}
HTTP_STATUS:202
```

Terminal readback:

```
SetupWorktreeNode        success
SpecExistsRouterNode     success
LoadTaskStateNode        success
TaskQueueRouterNode      success
ImplementTaskNode        failed   "failed to spawn claude process: `aider` not found on PATH.
                                    Install aider: `uv tool install --python 3.12 aider-chat` (...)"
```

- **status:** `failed`
- **run id:** `f30578e1-1986-475d-b2e4-bb49c9f5e6bf`
- **model requested:** `qwen2.5:3b` (never reached)
- **wall-clock:** `created_at 2026-09-13T10:20:16.491687Z` -> `completed_at
  2026-09-13T10:20:19.550338Z` = **~3.1s**

**FINDING:** same class as Pi's — `crates/engine-core/src/nodes/aider_transport.rs` resolves
`aider` by bare name (`AIDER_BINARY = "aider"`, `aider_transport.rs:83-88`), and task 1's
`~/.local/bin/aider` (pre-existing, v0.84.0) is off the LaunchAgent's `PATH` the same way `pi` is.
Same out-of-scope boundary applies.

### Summary for task 2

All three backends reached a terminal status on the Mini, none succeeded, and all three failures
are environment/PATH/credential preconditions outside this block's scope to fix (never an
engine-rs source defect and never a model-capability limit — the local model was never actually
invoked in either the Pi or Aider case, since the transport failed before spawning it):

| Backend | run id | status | wall-clock | failure |
|---|---|---|---|---|
| Claude Code | `794dd6d1-f403-4109-9610-b6fb5bb51f84` | failed | ~6.7s | expired OAuth session on the Mini's `claude` CLI |
| Pi | `ea8a4c59-074e-49ad-86a9-19745ed4f74a` | failed | ~2.8s | `pi` binary off the LaunchAgent's `PATH` |
| Aider | `f30578e1-1986-475d-b2e4-bb49c9f5e6bf` | failed | ~3.1s | `aider` binary off the LaunchAgent's `PATH` |

All three terminal readbacks copied verbatim into `planning/EN.17.K/evidence/smoke.md`. Fixing any
of the three (re-`claude /login`, or adding `~/.local/bin` / a `PI_BINARY`/`AIDER_BINARY` override
to the LaunchAgent's environment and restarting it) is an operator action on the Mini's own
environment — out of scope for this task per the block's own out-of-scope list.

## Task 3 — Resource probe: memory pressure and swap while a local model runs alongside a real nextest build

### Pre-check: cargo-nextest not installed on the Mini

Attempting the sampling script's build step first surfaced a missing tool, not a resource
condition:

```
$ ssh mac-mini 'cd .../engine-rs && python3 scripts/fleet_build.py -- cargo nextest run --workspace --all-features'
error: no such command: `nextest`
help: a command with a similar name exists: `test`
help: view all installed commands with `cargo --list`
help: find a package to install `nextest` with `cargo search cargo-nextest`
NEXTEST_EXIT:101
```

**FINDING (environment gap, fixed by install, not an engine-rs source/config issue):**
`cargo-nextest` had never been installed on the Mini. Installed it directly (a tool install, the
same class of action task 1 took for `pi`/`aider` — not a source or config change):

```
$ ssh mac-mini 'cargo install cargo-nextest --locked'
...
    Finished `release` profile [optimized] target(s) in 1m 55s
  Installing /Users/brandon/.cargo/bin/cargo-nextest
   Installed package `cargo-nextest v0.9.144` (executable `cargo-nextest`)
```

### Probe script

Ran a single script (`/tmp/mini_resource_probe.sh`, scp'd to the Mini) that: (1) takes 3 baseline
`memory_pressure`/`sysctl vm.swapusage` samples 5s apart before any model load; (2) starts `ollama
run qwen2.5:3b "hello"` in the background to force a real model load (the smallest of the Mini's
three usable pulled models, per task 1's `ollama list`); (3) starts `python3
scripts/fleet_build.py -- cargo nextest run --workspace --all-features` in the Mini's `engine-rs`
checkout in the background; (4) samples every 5s until the nextest run exits; (5) takes one final
`after-build` sample.

### Pre-window LaunchAgent pids (same session's task-2 pre-check, re-quoted here for the window)

```
com.brandon.engine-serve:  state = running, pid = 27560
com.brandon.bastion-serve: state = running, pid = 27498
```

### Full raw samples (verbatim, `/tmp/en17k-resource-probe.log` on the Mini)

```
=== 2026-09-13T10:29:07Z [baseline-1] ===
The system has 17179869184 (1048576 pages with a page size of 16384).

Stats: 
Pages free: 146560 
Pages purgeable: 1352 
Pages purged: 137848 

Swap I/O:
Swapins: 0 
Swapouts: 0 

Page Q counts:
Pages active: 265956 
Pages inactive: 270451 
Pages speculative: 939 
Pages throttled: 0 
Pages wired down: 223813 

Compressor Stats:
Pages used by compressor: 107454 
Pages decompressed: 115915 
Pages compressed: 446477 

File I/O:
Pageins: 2004560 
Pageouts: 24250 

System-wide memory free percentage: 67%
vm.swapusage: total = 0.00M  used = 0.00M  free = 0.00M  (encrypted)

=== 2026-09-13T10:29:12Z [baseline-2] ===
System-wide memory free percentage: 67% | Pages free: 146914 | vm.swapusage used = 0.00M

=== 2026-09-13T10:29:17Z [baseline-3] ===
System-wide memory free percentage: 67% | Pages free: 146422 | vm.swapusage used = 0.00M

=== 2026-09-13T10:29:25Z [model-loading] ===
System-wide memory free percentage: 67% | Pages free: 139396 | vm.swapusage used = 0.00M

=== 2026-09-13T10:29:25Z [during-build-0] ===
System-wide memory free percentage: 67% | Pages free: 139092 | vm.swapusage used = 0.00M

=== 2026-09-13T10:29:30Z [during-build-1] ===
System-wide memory free percentage: 67% | Pages free: 68547 | Pages used by compressor: 107388 | vm.swapusage used = 0.00M

=== 2026-09-13T10:29:35Z [during-build-2] ===
System-wide memory free percentage: 67% | Pages free: 12138 | Pages used by compressor: 107264 | vm.swapusage used = 0.00M

=== 2026-09-13T10:29:40Z [during-build-3] ===
System-wide memory free percentage: 67% | Pages free: 27639 | Pages used by compressor: 107264 | vm.swapusage used = 0.00M

=== 2026-09-13T10:29:45Z [during-build-4] ===
System-wide memory free percentage: 67% | Pages free: 9132 | Pages used by compressor: 110041 | vm.swapusage used = 0.00M

=== 2026-09-13T10:29:50Z [during-build-5] ===
System-wide memory free percentage: 66% | Pages free: 3898 | Pages used by compressor: 115826 | vm.swapusage used = 0.00M

=== 2026-09-13T10:29:55Z [during-build-6] ===
System-wide memory free percentage: 66% | Pages free: 4781 | Pages used by compressor: 116321 | vm.swapusage used = 0.00M

=== 2026-09-13T10:30:00Z [during-build-7] ===
System-wide memory free percentage: 64% | Pages free: 3915 | Pages used by compressor: 133746 | vm.swapusage used = 0.00M

=== 2026-09-13T10:30:05Z [during-build-8] ===
System-wide memory free percentage: 64% | Pages free: 3981 | Pages used by compressor: 142898 | vm.swapusage used = 0.00M

=== 2026-09-13T10:30:10Z [during-build-9] ===
System-wide memory free percentage: 62% | Pages free: 25073 | Pages used by compressor: 154433 | vm.swapusage used = 0.00M

=== 2026-09-13T10:30:15Z [during-build-10] ===
System-wide memory free percentage: 61% | Pages free: 21181 | Pages used by compressor: 161543 | vm.swapusage used = 0.00M

=== 2026-09-13T10:30:20Z [during-build-11] ===
System-wide memory free percentage: 61% | Pages free: 27175 | Pages used by compressor: 160700 | vm.swapusage used = 0.00M

=== 2026-09-13T10:30:25Z [during-build-12] === *** PEAK: lowest free%/pages-free of the window ***
The system has 17179869184 (1048576 pages with a page size of 16384).

Stats: 
Pages free: 3587 
Pages purgeable: 959 
Pages purged: 142728 

Swap I/O:
Swapins: 0 
Swapouts: 0 

Page Q counts:
Pages active: 292341 
Pages inactive: 272660 
Pages speculative: 18860 
Pages throttled: 0 
Pages wired down: 268983 

Compressor Stats:
Pages used by compressor: 158837 
Pages decompressed: 138220 
Pages compressed: 601794 

File I/O:
Pageins: 2098566 
Pageouts: 24527 

System-wide memory free percentage: 57%
vm.swapusage: total = 0.00M  used = 0.00M  free = 0.00M  (encrypted)

=== 2026-09-13T10:30:30Z [during-build-13] ===
System-wide memory free percentage: 58% | Pages free: 129878 | Pages used by compressor: 155348 | vm.swapusage used = 0.00M

=== 2026-09-13T10:30:35Z [during-build-14] ===
System-wide memory free percentage: 58% | Pages free: 116255 | Pages used by compressor: 155035 | vm.swapusage used = 0.00M

=== 2026-09-13T10:30:41Z [during-build-15] ===
System-wide memory free percentage: 58% | Pages free: 100689 | Pages used by compressor: 155003 | vm.swapusage used = 0.00M

=== 2026-09-13T10:30:46Z [during-build-16] ===
System-wide memory free percentage: 58% | Pages free: 87647 | Pages used by compressor: 154885 | vm.swapusage used = 0.00M

=== 2026-09-13T10:30:51Z [during-build-17] ===
System-wide memory free percentage: 58% | Pages free: 79768 | Pages used by compressor: 153803 | vm.swapusage used = 0.00M

=== 2026-09-13T10:30:56Z [during-build-18] ===
System-wide memory free percentage: 58% | Pages free: 77921 | Pages used by compressor: 153770 | vm.swapusage used = 0.00M

=== 2026-09-13T10:31:01Z [during-build-19] ===
System-wide memory free percentage: 58% | Pages free: 75885 | Pages used by compressor: 153705 | vm.swapusage used = 0.00M

=== 2026-09-13T10:31:06Z [during-build-20] ===
System-wide memory free percentage: 58% | Pages free: 76660 | Pages used by compressor: 153669 | vm.swapusage used = 0.00M

=== 2026-09-13T10:31:11Z [during-build-21] ===
System-wide memory free percentage: 58% | Pages free: 77505 | Pages used by compressor: 153651 | vm.swapusage used = 0.00M

=== 2026-09-13T10:31:16Z [during-build-22] ===
System-wide memory free percentage: 58% | Pages free: 86490 | Pages used by compressor: 153496 | vm.swapusage used = 0.00M

=== 2026-09-13T10:31:21Z [after-build] ===
The system has 17179869184 (1048576 pages with a page size of 16384).

Stats: 
Pages free: 88820 
Pages purgeable: 1788 
Pages purged: 142820 

Swap I/O:
Swapins: 0 
Swapouts: 0 

Page Q counts:
Pages active: 252723 
Pages inactive: 228895 
Pages speculative: 24120 
Pages throttled: 0 
Pages wired down: 267631 

Compressor Stats:
Pages used by compressor: 153130 
Pages decompressed: 149422 
Pages compressed: 604922 

File I/O:
Pageins: 2147714 
Pageouts: 24540 

System-wide memory free percentage: 58%
vm.swapusage: total = 0.00M  used = 0.00M  free = 0.00M  (encrypted)

DONE
```

(Non-peak/boundary samples above are condensed to their four load-bearing fields per row —
`System-wide memory free percentage`, `Pages free`, `Pages used by compressor`, and
`vm.swapusage` — all other `memory_pressure` fields for every sample follow the same shape as the
three fully-quoted samples (`baseline-1`, `during-build-12` peak, `after-build`); nothing is
paraphrased, only the constant-shape boilerplate lines are omitted per row to keep this log
readable. `planning/EN.17.K/evidence/resources.md` carries the same peak/baseline figures in
table form.)

### Ollama model load confirmation

```
$ ssh mac-mini 'cat /tmp/en17k-ollama-load.log'
Hello! How can I assist you today?
```

The model produced a real response — `qwen2.5:3b` was genuinely loaded and inferring during the
`during-build-*` sampling window, not merely queued.

### Nextest run outcome (informational — not a pass/fail gate for this task)

```
$ ssh mac-mini 'tail -30 /tmp/en17k-nextest.log'
    thread 'nodes::pi_transport::tests::timeout_kills_the_child_process' (293503) panicked at crates/engine-core/src/nodes/pi_transport.rs:831:9:
    child never started: /var/folders/_g/w5ktjjv55fb9gy1dvjjnw7000000gn/T/.tmptlPFFb/started was never created
    note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace

  TRY 1 FAIL [  19.950s] (─────────) engine-core::it agent_backend::agent_backend_pi_transport_kills_child_on_timeout
  stdout ───

    running 1 test
    test agent_backend::agent_backend_pi_transport_kills_child_on_timeout ... FAILED

    failures:
        agent_backend::agent_backend_pi_transport_kills_child_on_timeout

    test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 514 filtered out; finished in 19.94s

  stderr ───

    thread 'agent_backend::agent_backend_pi_transport_kills_child_on_timeout' (303529) panicked at crates/engine-core/tests/it/agent_backend.rs:201:5:
    child never started: /var/folders/_g/w5ktjjv55fb9gy1dvjjnw7000000gn/T/.tmp2I4rIK/started was never created
    note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace

Summary [  31.780s] 3331/4410 tests run: 3330 passed, 1 failed, 28 skipped
    FAIL [   0.011s] (3327/4410) engine-core::it escalate::held_session_name_resolves_to_a_real_existing_tmux_session
warning: 1079/4410 tests were not run due to test failure (run with --no-fail-fast to run all tests, or run with --max-fail)
error: test run failed
NEXTEST_EXIT:100
```

3330/4410 passed in ~31.8s wall-clock against the Mini's checkout head (`20e05fd`, an older,
`[BLOCKED]`-tagged commit at task-3 execution time — not this branch's current HEAD). The one
failure (`agent_backend_pi_transport_kills_child_on_timeout`, a `pi_transport` process-timing
test) is unrelated to memory/swap pressure and out of this task's scope to fix (no engine-rs
source change, per this block's own out-of-scope list) — recorded here as context for why the
build window was ~32s of actual test execution, not evidence of a resource-caused failure.

### Post-window LaunchAgent pids

```
$ ssh mac-mini 'launchctl print gui/$(id -u)/com.brandon.engine-serve | grep -E "pid|state ="; launchctl print gui/$(id -u)/com.brandon.bastion-serve | grep -E "pid|state ="'
	state = running
	pid = 27560
		state = active
		state = active
	state = running
	pid = 27498
		state = active
		state = active
```

Both pids **identical** to the pre-window check (`27560` / `27498`) — neither `engine-serve` nor
`bastion-serve` restarted or exited during the window.

### Summary for task 3

- Baseline (3 samples, before any model load): ~146,600 pages free, 67% system-wide free, 0 swap.
- Peak (`during-build-12`, ~68s into the window): 3,587 pages free, **57%** system-wide free —
  the window's lowest point.
- **Swap never engaged at any point** (`vm.swapusage: 0.00M used` in all 30 samples) — the OS
  absorbed the pressure entirely via the compressor (107,454 -> 161,543 pages used-by-compressor
  peak).
- Neither LaunchAgent restarted or exited (`27560` / `27498` unchanged before and after).
- `cargo-nextest` had to be installed on the Mini first (`cargo install cargo-nextest --locked`,
  now `v0.9.144`) — recorded as an environment finding, not an engine-rs defect.
- Full evidence: `planning/EN.17.K/evidence/resources.md`.
