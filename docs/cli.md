---
type: Reference
title: engine-rs CLI
description: Synopsis, subcommands, global flags, exit codes, and examples for engine-rs.
doc_id: cli
layer: [engine, console]
project: engine-rs
status: deprecated
keywords: [CLI, subcommands, exit codes, Rust, workflow runtime]
related: [docs-index, context, master-plan]
---

# engine-rs — CLI

> **This doc is retired.** engine-rs ships **no standalone binary and no CLI** — see
> [README.md](../README.md) "No standalone binary". It is a set of libraries (`engine-core`,
> `engine-serve`) meant to be embedded in a host process, currently `bastion serve`. Its surface
> is **HTTP**, not a command line: `engine-serve` mounts the HTTP routes every `engine-serve`
> workflow shares — the routes to trigger a run, stream its progress over server-sent events, and
> pause/resume/abort it — into whatever binary links it. See
> [architecture.md](architecture.md) for how that surface is built, and
> [workflows/README.md](workflows/README.md) for what each workflow does and how to trigger one
> over that surface. This page is kept only so existing links to `doc_id: cli` keep resolving; it
> carries no CLI documentation because there is no CLI to document.

## Synopsis

Not applicable — there is no `engine-rs` binary and therefore no invocation syntax.

## Subcommands

Not applicable — there is no `engine-rs` binary. Workflows are triggered over the HTTP surface
`engine-serve` mounts into its host process; see [workflows/README.md](workflows/README.md).

## Global Flags

Not applicable — there is no `engine-rs` binary.

## Exit Codes

Not applicable — there is no `engine-rs` binary. A workflow run's outcome is reported over the
HTTP surface (its status and any error), not a process exit code.

## Examples

Not applicable — there is no `engine-rs` binary to invoke. For a worked example of triggering and
observing a workflow run over the real HTTP surface, see [workflows/README.md](workflows/README.md).
