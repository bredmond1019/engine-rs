You are decomposing an already-authored block record into `tasks.json` — not drafting a plan
from scratch. The block record below is the source of truth for scope; you are producing the
executable task list `/sdlc-flow` and `/sdlc-task` run against it.

Rules, ported from `.claude/commands/generate-tasks.md`'s block-record-mode contract:

- **File ownership is disjoint UNLESS an explicit sequential dependency links the two tasks.**
  Two tasks that can run concurrently (no `dependsOn` edge between them) must never touch the
  same file. Two tasks may share a file only when the later task names the earlier task's
  `task_id` in its own `dependsOn` array — never two tasks with no such edge racing on the same
  path.
- **Every acceptance criterion must round-trip into a real `validation_commands` entry** — a
  concrete, runnable command that actually checks it, never left as bare prose copied from the
  block record.
- **`out_of_scope` is a hard boundary.** Do not generate a task for anything it names, even when
  it looks related to `what`.
- **Every task names at least one file in `files[]`, including the final task.** A task with
  `files: []` produces no diff and fails the engine's commit-derived work-assertion gate.
- **`task_id`s are 1-indexed, dependency-ordered, with no gaps**, and the final task's
  `dependsOn` includes every other task's id.
- **Do not invent work beyond what the block record's `what` and `files` describe.** Use the
  named files and `interfaces` as the reading list, not a license to expand scope.

Respond with strict JSON of the shape `{"tasks": [<SDLCTask>, ...], "tasks_markdown": "<rendered
tasks.md body>"}`, where each `<SDLCTask>` is `{task_id, title, description, acceptance_criteria,
validation_commands, max_attempts, files, dependsOn}`.

