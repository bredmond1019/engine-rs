You are decomposing a body of work into candidate block records for the `PLAN_AUTHORING`
workflow (`EN.19.B`). Follow `.claude/commands/plan.md`'s step-5 decomposition rules, carried here
VERBATIM as your operating rules:

- An initiative is typically 1-3 phases and 1-6 blocks. Much larger and it is a multi-repo
  program.
- A **block** is a coherent, independently reviewable unit that a task-generation step can turn
  into ~one spec. Not so large it hides separable concerns, not so small it fragments one feature.
- **Sequence by dependency and competence, not calendar.** Foundational, enabling work first; the
  hardest, most-differentiating work last.
- **Every block must ship something usable on its own.** Test each candidate: *what can the
  operator do the day this merges that they could not do the day before?* If the answer is
  "nothing yet," the block is mis-cut - merge it into the block that consumes it, or re-cut it so
  it lands with a visible surface however small.
- **A block that makes later work observable outranks a block that adds capability.** If the
  system cannot stop, report on, or verify its own work, every later block's failures are
  invisible.
- **Deletions come before the extensions that would inherit them.** If dead or superseded surface
  is in the way, cut a block that removes it first - it usually shrinks everything after it.
- Every block record must be self-sufficient: concrete **files** (new vs modified, by path),
  **observable acceptance criteria**, an explicit **out of scope**, and any shared **interfaces**.
- **Name files by path.** This is load-bearing, not decoration.
- **Distant blocks may be forward-looking** - author the full record while context is fresh, but
  expect files and interfaces to be refined when each becomes next.
- Do **not** bake stack, locale, or deployment specifics into blocks - those live in `CLAUDE.md` +
  `planning/harness.json`. Blocks are about *what*, *why*, *which files*, and *bounds*.

Respond with strict JSON of the shape `{"candidates": [<Candidate>, ...]}`, where each `<Candidate>`
carries the fields `id_placeholder`, `title`, `description`, `what`, `why`, `files`, `out_of_scope`,
`acceptance_criteria`. When you cannot fill a required field (`title`, `description`, `what`, `why`,
`files`, `out_of_scope`, `acceptance_criteria`) from the given input, OMIT that field rather than
inventing a plausible-looking value — an explicit gap is caught and marked `_incomplete: true` with
a `_missing_fields` list by the node that consumes this output; a fabricated value is not.
