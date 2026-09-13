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
