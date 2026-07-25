# Worklog — EN.5.D-policy-dispatch-seam

```
## Task 1 — PASSED (1 attempt)
What: Added crates/engine-core/src/policy/overlay.rs (Overlay trait, merge_opt, PartialLocalConfig, merge_local, and an Overlay impl for LocalConfig) and exported it from policy/mod.rs, giving the four workflow policy modules a shared merge surface to adopt in task 2.
Decisions: Kept policy::resolve::merge_opt as the sole publicly re-exported merge_opt from mod.rs (per the spec's 'keep resolve working exactly as today'); overlay::merge_opt exists as the relocated primitive but is not re-exported under the same name to avoid an ambiguous-glob/duplicate-name situation, since callers needing it can use policy::overlay::merge_opt directly.; Defined PartialLocalConfig and merge_local directly in overlay.rs (rather than only a trait) since all four existing workflow policy.rs modules already have byte-identical duplicates of this exact type/function — task 2 will delete those and import from here.
Validated: gating checks (fast tripwire)
```

## Task 2 — PASSED (1 attempt)
What: All four workflow policy.rs modules now delegate LocalConfig merging to the shared crate::policy::Overlay surface, use the shared merge_opt, and have the free-standing apply_override function inlined directly into each Policy::apply impl, with no hand-written merge_opt/merge_local/apply_override remaining anywhere under workflows/*/policy.rs.
Decisions: Kept each workflow's local PartialLocalConfig re-export as `pub use crate::policy::PartialLocalConfig;` (rather than removing the name) so existing sibling-module imports like `super::policy::PartialLocalConfig` (profiles.rs, revise.rs, review.rs) keep resolving without touching those files, since task 2's file list only covers the four policy.rs modules.; Inlined the former apply_override free function body directly into each type's Policy::apply method (binding `self` to `base`) rather than keeping it as a separate named function, since the task explicitly requires deleting `fn apply_override` while Policy::apply must still exist.; Left merge_model_tiers/merge_close_out/merge_close_out_reuse as private per-workflow free functions since they merge workflow-specific nested types not covered by the generic Overlay trait from task 1 (only LocalConfig has a shared Overlay impl).
Validated: gating checks (fast tripwire)

## Task 3 — PASSED (1 attempt)
What: crates/engine-core/src/policy/profiles.rs now exposes a PolicyConfigSource (Worktree/HarnessFile/Builtin) that decouples harness.json lookup from a worktree path, _from-suffixed siblings (read_harness_policy_defaults_from, read_harness_profiles_from, resolve_profile_from) built on it with the existing worktree-taking functions as thin wrappers, and a resolved_policy_strict read that errors instead of silently defaulting when the ResolvedPolicy stamp is absent or unparsable.
Decisions: Kept resolved_policy (lenient Default fallback) in place alongside the new resolved_policy_strict, per the task note that task 8 migrates callers and deletes the lenient one; PolicyConfigSource::harness_path() returns None only for Builtin, so read_*_from functions short-circuit before any filesystem access for that variant; Exported PolicyConfigSource and all new functions from policy/mod.rs alongside the existing public policy API
Validated: gating checks (fast tripwire)
