//! `EndReviewNode` — the drain-branch, end-of-run review
//! (`EN.ticket.review-mode-endonly-reviews-nothing`).
//!
//! `ReviewMode::EndOnly` documents itself (`policy.rs`, `profiles.rs`'s
//! `batch_reviewer` profile) as collapsing per-task review into a single
//! call over the whole run's accumulated diff. Before this node existed
//! that call never happened: `TriageRouterNode::route` (`task_loop.rs`)
//! routes every `PASS` verdict under `EndOnly` straight to
//! `UpdateTaskStatusNode`, skipping `ConsolidatedReviewNode`, and the drain
//! branch (`graph.rs`) had no review node at all — so under `EndOnly` no
//! acceptance criterion was ever read by a reviewer. This module is the
//! fix's model-calling half; the router that wires it into the drain
//! branch (`EndReviewRouterNode`) is a separate node added alongside it.
//!
//! **Active only under `ReviewMode::EndOnly`.** Under `PerTask` and
//! `TrivialSkip` [`EndReviewNode::process`] makes zero model calls and
//! passes `ctx` through unchanged (no `ctx.nodes` entry is written), so the
//! node's *presence* in the graph is unconditional (standing rule 6: a
//! policy knob must not change a declared graph's node set) while its
//! *behavior* is fully gated on the resolved policy.
//!
//! **Diff scope is the whole run, not one task's working-tree delta** — the
//! entire point (parity review §2.4 item 3: cross-task interactions and
//! criteria only satisfiable by the integrated tree get different verdicts
//! under per-task review). The base is `SetupWorktreeNode`'s recorded
//! `base_sha` (the commit the run's worktree/branch started from); when
//! that stamp is absent — a driven-directly unit test, or a failed
//! `git rev-parse` at setup — this falls back to `"main"`, the same base
//! `PullRequestNode` uses today for its `gh pr create --base main` call.
//!
//! **No retry loop on the drain branch.** JS's end review loops up to three
//! times with a fix pass between; that is a second implement/fix cycle over
//! the integrated tree and a materially larger change than this ticket
//! scopes. V1 issues exactly one review call and lets the router (added
//! alongside this node) turn a FAIL into a terminal, blocked run with a
//! named reason — a correct, legible outcome, and strictly better than the
//! silent zero-review status quo this fixes.

use std::path::Path;

use claude_code_rs::Config;
use engine_contract::TaskContext;
use serde_json::json;

use crate::node::{Node, NodeError};
use crate::nodes::{ClaudeCodeStep, MetaTransport};
use crate::routing::Router;

use super::policy::ReviewMode;
use super::task_loop::{
    apply_policy, bound_review_diff, latest_state, resolved_policy, review_output_schema,
    stage_untracked_intent, worktree_path, ReviewOutput, Stage, REVIEW_STABLE_PROMPT,
};
use super::{
    get_result, parse_structured_or_fenced, put_result, CommandRunner, ModelTransport,
    TransportSlot,
};

/// The result-node name [`EndReviewNode`] stamps under, and the name
/// `EndReviewRouterNode` (added alongside this node) routes on.
pub const NODE_NAME: &str = "EndReviewNode";

/// Fallback diff base when `SetupWorktreeNode` recorded no `base_sha` —
/// mirrors `PullRequestNode`'s hardcoded `--base main` (`pr.rs`). Neither
/// this node nor `PullRequestNode` reads `flow.prBase` from
/// `harness.json`: that block is not read anywhere in this workflow today,
/// which is its own parity gap and out of this ticket's scope.
const FALLBACK_DIFF_BASE: &str = "main";

/// Render every task's `acceptance_criteria` in the run's committed state
/// into one "## Acceptance Criteria" block, numbered by task so the model
/// can attribute a criterion back to the work that was supposed to satisfy
/// it. This IS the spec's complete Acceptance Criteria as far as the run
/// can see it — `SDLCState`/`SDLCTask` carry no separate spec-level AC
/// field; each task's list is the slice of the spec's AC that task owns,
/// and their union is the whole.
fn render_acceptance_criteria(tasks: &[super::schema::SDLCTask]) -> String {
    let mut out = String::from("## Acceptance Criteria\n");
    for task in tasks {
        out.push_str(&format!("\n### Task {}: {}\n", task.task_id, task.title));
        if task.acceptance_criteria.is_empty() {
            out.push_str("(no acceptance criteria recorded for this task)\n");
            continue;
        }
        for criterion in &task.acceptance_criteria {
            out.push_str(&format!("- {criterion}\n"));
        }
    }
    out
}

/// Deterministic-except-for-the-model-call node: under `ReviewMode::EndOnly`,
/// issues exactly one review call over the whole run's diff against the
/// complete Acceptance Criteria; under `PerTask`/`TrivialSkip`, a no-op
/// pass-through. See the module doc comment for the full rationale.
pub struct EndReviewNode {
    config: Config,
    transport: TransportSlot,
    runner: CommandRunner,
}

impl EndReviewNode {
    #[must_use]
    pub fn new() -> Self {
        Self {
            config: Config {
                model: Some("claude-sonnet-4-5".to_string()),
                ..Config::default()
            },
            transport: TransportSlot::default(),
            runner: super::default_command_runner(),
        }
    }

    /// Override the transport used by the composed `ClaudeCodeStep`.
    #[must_use]
    pub fn with_transport(mut self, transport: ModelTransport) -> Self {
        self.transport.set_plain(transport);
        self
    }

    /// Override the transport with a tier-aware [`MetaTransport`], taking
    /// precedence over a plain transport set via [`Self::with_transport`] —
    /// same precedence as `ConsolidatedReviewNode::with_meta_transport`.
    #[must_use]
    pub fn with_meta_transport(mut self, transport: MetaTransport) -> Self {
        self.transport.set_meta(transport);
        self
    }

    /// Override the command runner used for the `git diff` invocation.
    #[must_use]
    pub fn with_runner(mut self, runner: CommandRunner) -> Self {
        self.runner = runner;
        self
    }

    /// Override the base `Config` entirely (see
    /// `ConsolidatedReviewNode::with_config`'s doc comment for why this
    /// exists — live/manual tests driving a real `claude` call from inside
    /// another interactive session need `isolated: true`).
    #[must_use]
    pub fn with_config(mut self, config: Config) -> Self {
        self.config = config;
        self
    }
}

impl Default for EndReviewNode {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Node for EndReviewNode {
    async fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let policy = resolved_policy(&ctx)?;
        if policy.review_mode != ReviewMode::EndOnly {
            // Pass-through: zero model calls, `ctx` unchanged, no
            // `ctx.nodes["EndReviewNode"]` entry written. The node stays in
            // the graph's declared node set either way (standing rule 6) —
            // only its behavior is gated on the resolved policy.
            return Ok(ctx);
        }

        let worktree = worktree_path(&ctx)?;
        let worktree_dir = Path::new(&worktree);

        // Same intent-to-add-then-diff shape `ConsolidatedReviewNode` uses,
        // so brand-new files this run created appear with content instead
        // of being invisible to `git diff` as untracked paths.
        stage_untracked_intent(&self.runner, worktree_dir);

        let diff_base = get_result(&ctx, "SetupWorktreeNode")
            .and_then(|value| value.get("base_sha"))
            .and_then(|value| value.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| FALLBACK_DIFF_BASE.to_string());

        // `git diff <base>` (no `HEAD`) compares the base commit against the
        // current index + working tree, so it spans every task's commits
        // made so far on this branch AND any as-yet-uncommitted change —
        // the whole run's accumulated diff, not one task's delta.
        let diff = (self.runner)("git", &["diff", &diff_base], worktree_dir)
            .map(|output| output.stdout)
            .unwrap_or_default();

        let diff_budget = policy.review_diff_max_chars;
        let (diff, diff_truncated) = bound_review_diff(&diff, diff_budget as usize);

        let state = latest_state(&ctx)?;
        let acceptance_criteria = render_acceptance_criteria(&state.tasks);

        // Policy-varying text lives in the per-run prompt BODY only — never
        // in a `STABLE_SYSTEM_PROMPT` prefix (CLAUDE.md standing rule 6).
        let prompt = format!(
            "{REVIEW_STABLE_PROMPT}This is the END-OF-RUN review for spec \
             {spec_slug:?} — ONE review over the whole integrated tree, \
             replacing per-task review entirely. Review the WHOLE run's \
             accumulated diff against the COMPLETE Acceptance Criteria below \
             — every task's criteria, not just one task's. Respond with \
             strict JSON of the shape {{\"verdict\": str, \"summary\": str, \
             \"issues\": [str], \"localized\": bool}}. \"verdict\" must be \
             PASS, FAIL, or PARTIAL.\n\n{acceptance_criteria}\n\nDiff:\n{diff}",
            spec_slug = state.spec_slug,
        );

        let (mut config, prompt) =
            apply_policy(self.config.clone(), prompt, &policy, Stage::Review);
        config.cwd = Some(std::path::PathBuf::from(&worktree));
        config.json_schema = Some(review_output_schema());

        let step = self.transport.apply(
            ClaudeCodeStep::new(NODE_NAME, config, prompt)
                .with_retry_policy(policy.transport_retry),
        );

        let mut ctx = step.process(ctx).await?;
        let content = ctx
            .nodes
            .get(NODE_NAME)
            .and_then(|value| value.get("content"))
            .and_then(|value| value.as_str())
            .ok_or_else(|| NodeError::new(format!("{NODE_NAME}: model returned no content")))?
            .to_string();
        // Carry the transport stamp forward the same way
        // `ConsolidatedReviewNode` does — `put_result` below replaces this
        // node's whole `ctx.nodes` entry, which would otherwise silently
        // drop the `"transport"` stamp `ClaudeCodeStep::process` just wrote.
        let transport_stamp = ctx
            .nodes
            .get(NODE_NAME)
            .and_then(|value| value.get("transport"))
            .cloned();

        let parsed: ReviewOutput =
            parse_structured_or_fenced(&ctx, NODE_NAME, &content).map_err(|err| {
                NodeError::new(format!(
                    "{NODE_NAME}: failed to parse model output as JSON: {err}"
                ))
            })?;

        let normalized_verdict = parsed.verdict.trim().to_uppercase();
        let mut result = json!({
            "verdict": normalized_verdict,
            "summary": parsed.summary,
            "issues": parsed.issues,
            // See `ReviewOutput::localized` — stamped for the operator and
            // `/fix` routing; no Rust branch reads it, so this is
            // behavior-stable.
            "localized": parsed.localized,
            "review_diff_max_chars": diff_budget,
            "review_diff_truncated": diff_truncated,
        });
        if !matches!(normalized_verdict.as_str(), "PASS" | "FAIL" | "PARTIAL") {
            result["unrecognized_verdict"] = json!(normalized_verdict);
        }
        if let Some(transport) = transport_stamp {
            result["transport"] = transport;
        }
        put_result(&mut ctx, NODE_NAME, result);

        Ok(ctx)
    }

    fn name(&self) -> &str {
        NODE_NAME
    }
}

/// The drain-branch router wired directly after [`EndReviewNode`]:
/// `TaskQueueRouterNode -> FinalValidationNode -> EndReviewNode ->
/// EndReviewRouterNode -> {PatchDocsNode | WrapUpNode}` (`graph.rs`).
///
/// **Pass-through case (`PerTask`/`TrivialSkip`, or `EndOnly` PASS):**
/// routes to `PatchDocsNode` — the existing path, unchanged. This is also
/// the branch taken when `EndReviewNode` wrote no `ctx.nodes["EndReviewNode"]`
/// entry at all (the zero-call pass-through under `PerTask`/`TrivialSkip`),
/// so the graph's node set stays identical under every `review_mode`
/// (standing rule 6) while only `EndOnly` can ever route this router to
/// `WrapUpNode`.
///
/// **FAIL/PARTIAL:** routes to `WrapUpNode`. `wrap_up::derive_terminal_signal`
/// reads `EndReviewNode`'s stamped verdict independently (this router does
/// not stamp anything itself) and derives `TerminalSignal::EndReviewFail`,
/// so the run ends `blocked` with a `bail_reason` naming the unmet criteria
/// — an explicit `MAJOR_BAIL` from the task loop still wins over this, per
/// that function's checked-first ordering.
///
/// **No retry loop here** (see the module doc comment) — an unrecognized
/// verdict also routes to `WrapUpNode` (the same fallback-safety-net shape
/// `TriageRouterNode`/`ReviewRouterNode` use), matching
/// `wrap_up::derive_terminal_signal`'s `unrecognized_verdict` fallback arm.
pub struct EndReviewRouterNode;

#[async_trait::async_trait]
impl Node for EndReviewRouterNode {
    async fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError> {
        Ok(ctx)
    }

    fn name(&self) -> &str {
        "EndReviewRouterNode"
    }

    fn as_router(&self) -> Option<&dyn Router> {
        Some(self)
    }
}

impl Router for EndReviewRouterNode {
    fn route(&self, ctx: &TaskContext) -> Option<String> {
        let Some(review) = get_result(ctx, NODE_NAME) else {
            // EndReviewNode's zero-call pass-through under
            // `PerTask`/`TrivialSkip` writes no result — continue to the
            // existing drain path unchanged.
            return Some("PatchDocsNode".to_string());
        };
        let verdict = review.get("verdict").and_then(|v| v.as_str());
        match verdict {
            Some("PASS") => Some("PatchDocsNode".to_string()),
            // FAIL/PARTIAL and any unrecognized verdict all route to
            // WrapUpNode — the same catch-all shape ReviewRouterNode uses.
            _ => Some("WrapUpNode".to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflows::sdlc_flow::policy::SdlcPolicy;
    use crate::workflows::sdlc_flow::schema::{SDLCState, SDLCTask};
    use crate::workflows::sdlc_flow::CommandOutput;
    use claude_code_rs::Outcome;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    fn state_with_tasks(tasks: Vec<SDLCTask>) -> SDLCState {
        let mut state = SDLCState::new("EN.test.end-review");
        state.tasks = tasks;
        state
    }

    /// Mirrors `task_loop::tests::ctx_with_current_task`'s shape, minus the
    /// `TaskQueueRouterNode` stamp this node never reads: seeds
    /// `LoadTaskStateNode` (what [`latest_state`] looks for),
    /// `SetupWorktreeNode` (worktree + recorded `base_sha`), and the
    /// resolved policy.
    fn ctx_with_policy(policy_mode: ReviewMode, state: &SDLCState) -> TaskContext {
        let mut ctx = TaskContext {
            event: json!({}),
            nodes: HashMap::new(),
            metadata: json!({}),
            node_runs: HashMap::new(),
        };
        ctx.nodes.insert(
            "LoadTaskStateNode".to_string(),
            serde_json::to_value(state).expect("state serializes"),
        );
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": ".", "branch_name": "sdlc/test", "base_sha": "abc1234" }),
        );
        let policy = SdlcPolicy {
            review_mode: policy_mode,
            ..SdlcPolicy::default()
        };
        crate::policy::stamp_resolved_policy(&mut ctx, &policy).expect("policy stamps");
        ctx
    }

    fn make_runner(
        calls: Arc<Mutex<Vec<Vec<String>>>>,
        diff_output: &'static str,
    ) -> CommandRunner {
        Arc::new(move |program, args, _cwd| {
            calls.lock().unwrap().push(
                std::iter::once(program.to_string())
                    .chain(args.iter().map(|a| a.to_string()))
                    .collect(),
            );
            if program == "git" && args.first().copied() == Some("diff") {
                Ok(CommandOutput {
                    status: 0,
                    stdout: diff_output.to_string(),
                    stderr: String::new(),
                })
            } else {
                Ok(CommandOutput {
                    status: 0,
                    stdout: String::new(),
                    stderr: String::new(),
                })
            }
        })
    }

    fn canned_outcome(text: String) -> Outcome {
        Outcome {
            cost_usd: 0.0,
            usage: claude_code_rs::parse::Usage {
                input_tokens: 1,
                output_tokens: 1,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
            },
            model_usage: std::collections::BTreeMap::new(),
            text,
            is_error: false,
            api_error_status: None,
            session_id: None,
            structured_output: None,
        }
    }

    fn model_transport_returning(reply_json: &'static str) -> ModelTransport {
        Arc::new(move |_config, _prompt| {
            let outcome = canned_outcome(reply_json.to_string());
            Box::pin(async move { Ok(outcome) })
        })
    }

    #[tokio::test]
    async fn per_task_mode_makes_zero_calls_and_passes_through_unchanged() {
        let state = state_with_tasks(vec![SDLCTask::new(1, "t1", "d1")]);
        let ctx = ctx_with_policy(ReviewMode::PerTask, &state);
        let calls = Arc::new(Mutex::new(Vec::new()));
        let runner = make_runner(calls.clone(), "diff content");
        let model_calls = Arc::new(Mutex::new(0u32));
        let model_calls_clone = model_calls.clone();
        let transport: ModelTransport = Arc::new(move |_config, _prompt| {
            *model_calls_clone.lock().unwrap() += 1;
            let outcome = canned_outcome(
                "{\"verdict\":\"PASS\",\"summary\":\"s\",\"issues\":[]}".to_string(),
            );
            Box::pin(async move { Ok(outcome) })
        });

        let before = ctx.nodes.len();
        let node = EndReviewNode::new()
            .with_runner(runner)
            .with_transport(transport);
        let out = node.process(ctx).await.expect("process should succeed");

        assert_eq!(
            *model_calls.lock().unwrap(),
            0,
            "PerTask must make zero model calls"
        );
        assert!(
            calls.lock().unwrap().is_empty(),
            "PerTask must run no git commands"
        );
        assert!(!out.nodes.contains_key(NODE_NAME));
        assert_eq!(out.nodes.len(), before, "ctx.nodes must be unchanged");
    }

    #[tokio::test]
    async fn trivial_skip_mode_makes_zero_calls_and_passes_through_unchanged() {
        let state = state_with_tasks(vec![SDLCTask::new(1, "t1", "d1")]);
        let ctx = ctx_with_policy(ReviewMode::TrivialSkip, &state);
        let calls = Arc::new(Mutex::new(Vec::new()));
        let runner = make_runner(calls.clone(), "diff content");

        let node = EndReviewNode::new().with_runner(runner);
        let out = node.process(ctx).await.expect("process should succeed");

        assert!(
            calls.lock().unwrap().is_empty(),
            "TrivialSkip must run no git commands"
        );
        assert!(!out.nodes.contains_key(NODE_NAME));
    }

    #[tokio::test]
    async fn end_only_mode_makes_exactly_one_call_with_full_criteria_and_multi_task_diff() {
        let mut task1 = SDLCTask::new(1, "Task One", "d1");
        task1.acceptance_criteria = vec!["criterion one".to_string()];
        let mut task2 = SDLCTask::new(2, "Task Two", "d2");
        task2.acceptance_criteria = vec!["criterion two".to_string()];
        let state = state_with_tasks(vec![task1, task2]);
        let ctx = ctx_with_policy(ReviewMode::EndOnly, &state);

        let calls = Arc::new(Mutex::new(Vec::new()));
        let runner = make_runner(
            calls.clone(),
            "diff --git a/one.rs b/one.rs\n+task one change\n\
             diff --git a/two.rs b/two.rs\n+task two change\n",
        );

        let captured_prompt = Arc::new(Mutex::new(String::new()));
        let captured_prompt_clone = captured_prompt.clone();
        let model_calls = Arc::new(Mutex::new(0u32));
        let model_calls_clone = model_calls.clone();
        let transport: ModelTransport = Arc::new(move |_config, prompt| {
            *model_calls_clone.lock().unwrap() += 1;
            *captured_prompt_clone.lock().unwrap() = prompt;
            let outcome = canned_outcome(
                "{\"verdict\":\"PASS\",\"summary\":\"looks good\",\"issues\":[]}".to_string(),
            );
            Box::pin(async move { Ok(outcome) })
        });

        let node = EndReviewNode::new()
            .with_runner(runner)
            .with_transport(transport);
        let out = node.process(ctx).await.expect("process should succeed");

        assert_eq!(
            *model_calls.lock().unwrap(),
            1,
            "EndOnly must make exactly one call"
        );
        let prompt = captured_prompt.lock().unwrap().clone();
        assert!(
            prompt.contains("criterion one"),
            "prompt must carry task 1's AC"
        );
        assert!(
            prompt.contains("criterion two"),
            "prompt must carry task 2's AC"
        );
        assert!(
            prompt.contains("task one change"),
            "diff must span task 1's change"
        );
        assert!(
            prompt.contains("task two change"),
            "diff must span task 2's change"
        );

        let git_calls = calls.lock().unwrap();
        let expected: Vec<String> = vec!["git", "diff", "abc1234"]
            .into_iter()
            .map(String::from)
            .collect();
        assert!(
            git_calls.iter().any(|c| c == &expected),
            "must diff against the recorded base_sha, got {git_calls:?}"
        );

        let result = &out.nodes[NODE_NAME];
        assert_eq!(result["verdict"], json!("PASS"));
        assert_eq!(result["summary"], json!("looks good"));
    }

    /// `EN.ticket.sdlc-flow-dead-policy-knobs` task 3: a non-default
    /// `transport_retry` on the resolved policy changes the observed
    /// attempt count against a persistently failing transport for
    /// `EndReviewNode`.
    #[tokio::test]
    async fn end_only_mode_transport_retry_nondefault_changes_observed_attempts() {
        let state = state_with_tasks(vec![SDLCTask::new(1, "t1", "d1")]);
        let mut ctx = TaskContext {
            event: json!({}),
            nodes: HashMap::new(),
            metadata: json!({}),
            node_runs: HashMap::new(),
        };
        ctx.nodes.insert(
            "LoadTaskStateNode".to_string(),
            serde_json::to_value(&state).expect("state serializes"),
        );
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": ".", "branch_name": "sdlc/test", "base_sha": "abc1234" }),
        );
        let policy = SdlcPolicy {
            review_mode: ReviewMode::EndOnly,
            transport_retry: crate::workflows::sdlc_flow::policy::TransportRetry {
                max_attempts: 4,
                initial_backoff_ms: 0,
            },
            ..SdlcPolicy::default()
        };
        crate::policy::stamp_resolved_policy(&mut ctx, &policy).expect("policy stamps");

        let calls = Arc::new(Mutex::new(Vec::new()));
        let runner = make_runner(calls.clone(), "diff content");
        let attempts = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let transport: ModelTransport = Arc::new({
            let attempts = attempts.clone();
            move |_config, _prompt| {
                let attempts = attempts.clone();
                Box::pin(async move {
                    attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Err(claude_code_rs::Error::Timeout)
                })
            }
        });

        let node = EndReviewNode::new()
            .with_runner(runner)
            .with_transport(transport);
        let result = node.process(ctx).await;
        assert!(
            result.is_err(),
            "persistent failure must still halt the walk"
        );
        assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 4);
    }

    #[tokio::test]
    async fn end_only_mode_falls_back_to_main_when_base_sha_absent() {
        let state = state_with_tasks(vec![SDLCTask::new(1, "t1", "d1")]);
        let mut ctx = ctx_with_policy(ReviewMode::EndOnly, &state);
        // Overwrite SetupWorktreeNode's stamp to omit base_sha.
        ctx.nodes.insert(
            "SetupWorktreeNode".to_string(),
            json!({ "worktree_path": ".", "branch_name": "sdlc/test" }),
        );

        let calls = Arc::new(Mutex::new(Vec::new()));
        let runner = make_runner(calls.clone(), "diff content");
        let transport =
            model_transport_returning("{\"verdict\":\"PASS\",\"summary\":\"s\",\"issues\":[]}");

        let node = EndReviewNode::new()
            .with_runner(runner)
            .with_transport(transport);
        let _ = node.process(ctx).await.expect("process should succeed");

        let git_calls = calls.lock().unwrap();
        let expected: Vec<String> = vec!["git", "diff", "main"]
            .into_iter()
            .map(String::from)
            .collect();
        assert!(
            git_calls.iter().any(|c| c == &expected),
            "must fall back to `main` when base_sha is absent, got {git_calls:?}"
        );
    }

    #[tokio::test]
    async fn end_only_mode_truncates_an_oversized_diff_with_the_visible_banner() {
        let state = state_with_tasks(vec![SDLCTask::new(1, "t1", "d1")]);
        let mut ctx = ctx_with_policy(ReviewMode::EndOnly, &state);
        let policy = SdlcPolicy {
            review_mode: ReviewMode::EndOnly,
            review_diff_max_chars: 50,
            ..SdlcPolicy::default()
        };
        crate::policy::stamp_resolved_policy(&mut ctx, &policy).expect("policy stamps");

        let huge_diff: &'static str = Box::leak("x".repeat(500).into_boxed_str());
        let calls = Arc::new(Mutex::new(Vec::new()));
        let runner = make_runner(calls.clone(), huge_diff);

        let captured_prompt = Arc::new(Mutex::new(String::new()));
        let captured_prompt_clone = captured_prompt.clone();
        let transport: ModelTransport = Arc::new(move |_config, prompt| {
            *captured_prompt_clone.lock().unwrap() = prompt;
            let outcome = canned_outcome(
                "{\"verdict\":\"PARTIAL\",\"summary\":\"s\",\"issues\":[]}".to_string(),
            );
            Box::pin(async move { Ok(outcome) })
        });

        let node = EndReviewNode::new()
            .with_runner(runner)
            .with_transport(transport);
        let out = node.process(ctx).await.expect("process should succeed");

        let prompt = captured_prompt.lock().unwrap().clone();
        assert!(
            prompt.contains("DIFF TRUNCATED"),
            "oversized diff must carry the visible truncation banner"
        );
        assert_eq!(out.nodes[NODE_NAME]["review_diff_truncated"], json!(true));
    }

    #[test]
    fn render_acceptance_criteria_unions_every_task() {
        let mut task1 = SDLCTask::new(1, "Task One", "d1");
        task1.acceptance_criteria = vec!["a".to_string(), "b".to_string()];
        let mut task2 = SDLCTask::new(2, "Task Two", "d2");
        task2.acceptance_criteria = vec!["c".to_string()];

        let rendered = render_acceptance_criteria(&[task1, task2]);

        assert!(rendered.contains("Task 1: Task One"));
        assert!(rendered.contains("- a"));
        assert!(rendered.contains("- b"));
        assert!(rendered.contains("Task 2: Task Two"));
        assert!(rendered.contains("- c"));
    }

    // --- EndReviewRouterNode -------------------------------------------

    fn ctx_with_end_review_result(value: serde_json::Value) -> TaskContext {
        let mut ctx = TaskContext {
            event: json!({}),
            nodes: HashMap::new(),
            metadata: json!({}),
            node_runs: HashMap::new(),
        };
        ctx.nodes.insert(NODE_NAME.to_string(), value);
        ctx
    }

    #[test]
    fn router_passes_through_to_patch_docs_when_end_review_never_ran() {
        // The PerTask/TrivialSkip pass-through case: EndReviewNode wrote no
        // ctx.nodes entry, so the router must not treat that as a FAIL.
        let ctx = TaskContext {
            event: json!({}),
            nodes: HashMap::new(),
            metadata: json!({}),
            node_runs: HashMap::new(),
        };
        let router = EndReviewRouterNode;
        assert_eq!(router.route(&ctx), Some("PatchDocsNode".to_string()));
    }

    #[test]
    fn router_routes_pass_to_patch_docs() {
        let ctx = ctx_with_end_review_result(json!({
            "verdict": "PASS",
            "summary": "all good",
            "issues": [],
        }));
        let router = EndReviewRouterNode;
        assert_eq!(router.route(&ctx), Some("PatchDocsNode".to_string()));
    }

    #[test]
    fn router_routes_fail_to_wrap_up() {
        let ctx = ctx_with_end_review_result(json!({
            "verdict": "FAIL",
            "summary": "criteria unmet",
            "issues": ["criterion X not met"],
        }));
        let router = EndReviewRouterNode;
        assert_eq!(router.route(&ctx), Some("WrapUpNode".to_string()));
    }

    #[test]
    fn router_routes_partial_to_wrap_up() {
        let ctx = ctx_with_end_review_result(json!({
            "verdict": "PARTIAL",
            "summary": "some criteria unmet",
            "issues": ["criterion Y not met"],
        }));
        let router = EndReviewRouterNode;
        assert_eq!(router.route(&ctx), Some("WrapUpNode".to_string()));
    }

    #[test]
    fn router_routes_unrecognized_verdict_to_wrap_up() {
        let ctx = ctx_with_end_review_result(json!({
            "verdict": "WAT",
            "summary": "",
            "issues": [],
            "unrecognized_verdict": "WAT",
        }));
        let router = EndReviewRouterNode;
        assert_eq!(router.route(&ctx), Some("WrapUpNode".to_string()));
    }
}
