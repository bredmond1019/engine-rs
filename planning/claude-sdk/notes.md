---
type: Note
title: Claude SDK Transport Decision & Audit
description: Notes and findings comparing homegrown claude-sdk-rs with official Python SDK and native Rust SDK.
---

# Claude SDK Audit & Transport Decision

This document captures the findings of our evaluation into how to integrate Claude Code into `engine-rs` (block EN.2.A). Our primary constraint is **utilizing the flat-rate Claude Code Subscription** rather than paying for Anthropic API credits.

## 1. How the Official Python SDK Uses the Subscription
**Repo:** `/Users/brandon/Dev/agentic-portfolio/core/reference-repos/claude-agent-sdk-python`

Surprisingly, the official Anthropic Python SDK does **not** hit an API natively when resolving Claude Code sessions. Instead, it acts as a headless wrapper around the `claude` CLI subprocess, identical in concept to our homegrown SDK.

Here is exactly how it authenticates using your subscription without API credits:
- **File:** `src/claude_agent_sdk/_internal/session_resume.py`
- **Keychain Interception (`_read_keychain_credentials`)**: It runs a macOS command (`security find-generic-password -a <user> -w -s 'Claude Code-credentials'`) to quietly extract the OAuth token that the `claude login` command saved to your keychain.
- **Environment Isolation (`_copy_auth_files`)**: It creates a temporary directory (e.g., `/tmp/claude-resume-xxx`) mimicking the `~/.claude/` layout.
- **Redacting the Refresh Token (`_write_redacted_credentials`)**: It dumps the OAuth token into `.credentials.json` inside that temporary directory, but it explicitly **deletes the `refreshToken` field**. This is critical—if the subprocess refreshed the token, it would invalidate the parent's token and log you out.
- **Execution**: It sets `CLAUDE_CONFIG_DIR=/tmp/...` and executes the `claude` CLI binary as a child process.

## 2. Our Homegrown SDK (`claude-sdk-rs`)
**Repo:** `/Users/brandon/Dev/agentic-portfolio/portfolio/claude-sdk-rs`

Our custom Rust SDK correctly identified the subprocess approach (using `tokio::process::Command` to run `claude -p`) as the right seam. However, it suffers from several lifecycle and parsing gaps.

### The Good
- `src/runtime/process.rs`: The `execute_claude` function correctly constructs the arguments and forces non-interactive mode (`-p`).
- The `Config` struct is a good pattern for wrapping the CLI flags (`--system-prompt`, `--allowedTools`, etc.).
- Concurrency works out of the box since every query spawns an independent subprocess.

### The Pitfalls & Gaps
- **Orphaned Zombie Processes (Critical)**: `execute_claude` and `execute_claude_streaming` rely on `tokio::time::timeout`, but they fail to call `Command::kill_on_drop(true)`. When a timeout hits, the future drops but the `claude` CLI keeps running in the background indefinitely, racking up bills.
- **Broken Typed Streaming**: The CLI's internal `--output-format stream-json` schema changes constantly without warning. The manual parsing in `src/runtime/stream.rs` is tightly coupled to an old format, causing all streaming events to silently fail parsing and drop.
- **Cost/Token Drift**: It attempts to read `cost_usd` from the JSON output (`src/core/types.rs`), but the CLI has updated this to `total_cost_usd` and moved `usage` into a nested object. It currently silently returns `None`.
- **Dead Code**: `src/core/session/session.rs` contains a heavy `SessionManager` that isn't even wired up to the execution path.

## 3. The Native Reference SDK (`claude-agent-sdk-rust`)
**Repo:** `/Users/brandon/Dev/agentic-portfolio/core/reference-repos/claude-agent-sdk-rust`

This is an idiomatic native API client that talks directly to `api.anthropic.com`. Because it requires an `ANTHROPIC_API_KEY`, it **consumes API credits and bypasses the subscription**, making it unsuitable as a drop-in transport. However, its architecture has excellent pieces we can borrow.

### Useful Components to Pull
- **Fluent Session Management**: `src/conversation.rs` provides an excellent `ConversationBuilder` pattern for managing multi-turn state cleanly, which we could adapt for `--resume` IDs or state passing.
- **Robust SSE Streaming**: `src/streaming.rs` utilizes `eventsource-stream` to correctly parse Server-Sent Events. We could adapt this if we figure out how to force the CLI to emit standard SSE.
- **Token Counting**: `src/tokens.rs` uses `tiktoken-rs` for local offline token counting, accurately measuring message and tool overhead before even sending a request.
- **Strong Typing**: `src/types.rs` makes great use of an `Unknown` enum variant for unrecognized content blocks, guaranteeing forward compatibility if Anthropic adds new types.

## 4. Goal & Options for `engine-rs` (Block EN.2.A)
We must decide between three paths for the `ClaudeCodeStep` node:

### Option A: Clean up `claude-sdk-rs`
- **Pros**: It's an isolated library we control. We just add `.kill_on_drop(true)`, fix the `total_cost_usd` parsing, and rip out the broken streaming and dead session code.
- **Cons**: We have to maintain a separate crate whose only real purpose is wrapping `std::process::Command`.

### Option B: Hybrid (CLI wrapper + Native SDK parts)
- **Pros**: We bring in the `ConversationBuilder` and `TokenCounter` from the reference Rust SDK, but use the subprocess logic from our homegrown SDK to preserve the subscription.
- **Cons**: High effort. We'd be marrying a clean API SDK to a dirty CLI subprocess wrapper.

### Option C: Write a Simpler Node directly in `engine-rs`
- **Pros**: Simplest architecture. We build `crates/engine-core/src/nodes/claude_code_step.rs` that directly spawns `tokio::process::Command::new("claude")` with `kill_on_drop(true)`. We can implement the keychain interception trick from Python to guarantee clean auth. We parse the raw JSON directly in the node to get `total_cost_usd`.
- **Cons**: Less abstracted, ties the engine directly to the CLI subprocess logic.

**Must-Haves for any choice**:
- Async execution.
- Hard process cancellation (`kill_on_drop`).
- Accurate cost reporting (via `total_cost_usd` from the CLI output).
