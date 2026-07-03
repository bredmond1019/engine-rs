# Commit — Stage and commit changes with a conventional message.

## Variables

$ARGUMENTS — optional commit message override or scope hint.

## Execution Model

Spawn a Haiku subagent (Agent tool, `model: "haiku"`) to execute all steps below.
Pass the resolved `$ARGUMENTS` value and the complete Instructions section in the subagent prompt.
Return the subagent's result to the user.

## Instructions

1. Run `git status` and `git diff --stat` to see what changed.
2. Determine the commit strategy:
   - **Only code changed** (no `planning/` or `log.md`): one code commit.
   - **Only planning/docs changed** (`planning/`, `log.md`, `*.md`): one `docs:` commit.
   - **Both changed**: two commits — code first, then docs.
3. Draft a short conventional commit message:
   - Format: `<type>(<scope>): <summary>` — e.g. `feat(auth): add token refresh`, `docs: update status.md for block 2`
   - Types: `feat`, `fix`, `refactor`, `test`, `docs`, `chore`
   - Keep the summary under 72 characters.
   - If `$ARGUMENTS` is provided, use it as the message or incorporate it as the scope/summary.
4. Show the staged files and proposed message to the user and ask for confirmation before committing.
5. On confirmation, stage the relevant files and commit. Do not use `git add -A` — add files explicitly by name.
6. Do not push. Do not use `--no-verify`.

## Context / Files to Read

None — this command runs git commands only.
