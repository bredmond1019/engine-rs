//! `BrandCriticNode` — the `critic`-stage model node gating every drafted
//! LinkedIn post candidate against `agentic-portfolio/business/docs/brand.md`'s
//! anti-slop bank (`planning/EN.5.G/tasks.md` + `tasks.json` task 5 — "this
//! is the point of the block").
//!
//! Mirrors `content_pipeline::self_critic::SelfCriticNode`'s shape and its
//! `CriticEvaluation` `{verdict, confidence, issues[]}` contract (reused
//! directly from `content_pipeline::schema` rather than redefined here — a
//! second, drifting copy of the same contract would be exactly the kind of
//! duplication CLAUDE.md rule 6's shape-invariance discipline warns
//! against), but the rubric is brand.md's six-check banned-construction
//! bank, carried **verbatim**.
//!
//! ## Two enforcement layers, not one
//!
//! Three of the six checks are mechanically detectable from the draft text
//! alone — no model judgment required, and this block's stated acceptance
//! criteria test exactly these three:
//! - (1) rhetorical contrast setups ("Not X, it's Y." / "That gap between
//!   X and Y." / "This isn't just X, it's Y.")
//! - (2) bold-label bullet triplets (`**Bold phrase** — explanation`,
//!   three times running)
//! - (5) stacked em-dashes (more than one aside in one paragraph)
//!
//! [`deterministic_scan`] catches these **before** any model call — a
//! fixture draft containing one of these constructions is caught
//! reliably, not contingent on how a stubbed transport happens to reply.
//! The remaining three checks — (3) hedge phrases, (4) summary filler, (6)
//! the read-aloud test — are voice judgments a pattern cannot safely make
//! (a "typically" that is load-bearing technical qualification isn't
//! automatically a hedge-word violation), so those are left to the model,
//! which receives the full six-check rubric verbatim in [`RUBRIC`] and is
//! asked to judge only what the deterministic scan did not already catch.
//!
//! ## Fail-closed, same precedent as `SelfCriticNode`
//!
//! An ambiguous/malformed model verdict normalizes to `Revise`
//! ([`verdict_from_model_text`], identical to `self_critic.rs`'s own
//! function) — an unclear signal must never let the loop exit early on a
//! false `Pass`.
//!
//! ## The iteration-cap failing marker (task 5 AC5)
//!
//! A draft still failing when this is the terminal pass
//! (`iteration + 1 >= policy.max_critic_iterations`) is stamped with an
//! explicit `"capped": true` marker alongside the `CriticEvaluation` JSON —
//! so a draft that never passes is distinguishable from an ordinary
//! mid-loop `Revise` by inspecting `ctx.nodes[NODE_NAME]["capped"]`, not by
//! inferring it from the loop having simply stopped calling this node. This
//! keeps AC5 satisfiable at this node's own boundary, independent of how
//! task 6 wires the surrounding router.

use engine_contract::TaskContext;
use serde::Deserialize;
use serde_json::json;

use crate::node::{InputBinding, Node, NodeError};
use crate::nodes::ClaudeCodeStep;
use crate::workflows::content_pipeline::increment_critic_iteration;
use crate::workflows::content_pipeline::schema::{CriticEvaluation, CriticVerdict};
use crate::workflows::{get_result, parse_structured_or_fenced, put_result, ModelTransport};

use super::policy::LinkedInPostPolicy;
use super::{draft, revise};

/// The `Node::name()` identity `BrandCriticNode` runs its composed
/// `ClaudeCodeStep` under, and the `ctx.nodes` key its output is stamped
/// onto. Read by `ReviseNode` (`revise.rs`) and, once task 6 wires it, the
/// linkedin_post critic router.
pub const NODE_NAME: &str = "BrandCriticNode";

/// brand.md's "Voice — avoiding AI slop" bank, carried **verbatim** — lines
/// 178-190 of `agentic-portfolio/business/docs/brand.md` at this task's
/// authoring time. Do not paraphrase; a rewritten rubric is not the load-
/// bearing artifact this block exists to ship.
pub const RUBRIC: &str = "\
- Rhetorical contrast setups. \"Not X, it's Y.\" \"That gap between X and Y.\" \"This isn't just X, \
it's Y.\" These are LLM tics, not how Brandon talks. State the thing plainly instead.
- Bold-label bullet triplets. **Bold phrase** — explanation, repeated three times in a row, is a \
dead giveaway of generated text. Vary the structure — prose paragraphs, a mix of short and long \
bullets, not a symmetric grid every time.
- Hedge phrases. \"I've generally found,\" \"in my experience,\" \"typically.\" Say the specific \
thing instead of softening it with a hedge word.
- Summary filler. \"That's the X piece.\" \"That's the thing that matters here.\" Cut any sentence \
that exists only to restate what the previous sentence already said.
- Stacked em-dashes. One aside per paragraph, not three. Reach for a period or a comma first.
- Read-aloud test. If it doesn't sound like something Brandon would actually say to someone he \
knows — casual but professional, teacher first, developer second, business person third — rewrite \
it.";

/// The model's reply shape for the checks the deterministic scan cannot
/// safely make (hedge phrases, summary filler, the read-aloud test).
#[derive(Debug, Clone, Deserialize)]
struct CriticOutput {
    verdict: String,
    #[serde(default)]
    confidence: f64,
    #[serde(default)]
    issues: Vec<String>,
}

/// JSON schema matching [`CriticOutput`].
fn critic_output_json_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "verdict": { "type": "string", "enum": ["pass", "revise"] },
            "confidence": { "type": "number" },
            "issues": { "type": "array", "items": { "type": "string" } },
        },
        "required": ["verdict"],
    })
}

/// Normalize a model's free-form verdict text into a `CriticVerdict`,
/// defaulting to `Revise` (fail closed) unless the text unambiguously
/// reads as a pass. Identical to `content_pipeline::self_critic`'s own
/// `verdict_from_model_text`.
fn verdict_from_model_text(text: &str) -> CriticVerdict {
    if text.trim().to_lowercase().starts_with("pass") {
        CriticVerdict::Pass
    } else {
        CriticVerdict::Revise
    }
}

/// One finding from [`deterministic_scan`] — the check it violates plus a
/// human-readable issue string naming that check, so the returned `issues`
/// list is legible on its own (matches the "with an issue naming the
/// contrast setup" / "naming the bullet triplet" phrasing in the block's
/// acceptance criteria).
struct ScanFinding {
    issue: String,
}

/// Rhetorical contrast setups (check 1) — the three constructions named
/// verbatim in brand.md: "Not X, it's Y.", "That gap between X and Y.",
/// "This isn't just X, it's Y." Matched case-insensitively on the
/// characteristic fragments rather than a full sentence template, since
/// brand.md itself gives these as illustrative phrasings, not an exhaustive
/// enumeration.
fn scan_rhetorical_contrast(draft: &str) -> Option<ScanFinding> {
    let lower = draft.to_lowercase();
    let hit = lower.contains("isn't just")
        || lower.contains("is not just")
        || lower.contains("that gap between")
        || (lower.contains("not ") && lower.contains(", it's "));
    if hit {
        Some(ScanFinding {
            issue:
                "rhetorical contrast setup: a \"Not X, it's Y\" / \"This isn't just X, it's Y\" \
                    / \"That gap between X and Y\" construction — state the thing plainly instead."
                    .to_string(),
        })
    } else {
        None
    }
}

/// Bold-label bullet triplets (check 2) — three consecutive Markdown
/// bullets each opening `**Bold phrase** — explanation` (or `- **Bold** -
/// explanation`). Scans line-by-line for a bullet whose leading `**...**`
/// label is immediately followed by a dash, and flags a run of 3+ such
/// lines in a row (blank lines break a run, matching "three times in a
/// row").
fn scan_bullet_triplet(draft: &str) -> Option<ScanFinding> {
    let mut run = 0u32;
    for line in draft.lines() {
        let trimmed = line.trim_start();
        let bullet = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
            .unwrap_or(trimmed);
        let is_bold_label_bullet = bullet.starts_with("**")
            && bullet[2..]
                .find("**")
                .map(|end| {
                    let after = bullet[2 + end + 2..].trim_start();
                    after.starts_with('—') || after.starts_with('-') || after.starts_with('–')
                })
                .unwrap_or(false);
        if is_bold_label_bullet {
            run += 1;
            if run >= 3 {
                return Some(ScanFinding {
                    issue: "bold-label bullet triplet: three consecutive `**Bold phrase** — \
                            explanation` bullets — vary the structure instead of a symmetric grid."
                        .to_string(),
                });
            }
        } else if !trimmed.is_empty() {
            run = 0;
        }
    }
    None
}

/// Stacked em-dashes (check 5) — more than one em-dash aside in a single
/// paragraph (a paragraph is text between blank lines). Counts both the
/// true em-dash (`—`) and the double-hyphen ASCII substitute (`--`) some
/// drafts use in its place.
fn scan_stacked_em_dashes(draft: &str) -> Option<ScanFinding> {
    for paragraph in draft.split("\n\n") {
        let count = paragraph.matches('—').count() + paragraph.matches("--").count();
        if count > 1 {
            return Some(ScanFinding {
                issue: "stacked em-dashes: more than one aside in one paragraph — reach for a \
                        period or a comma first."
                    .to_string(),
            });
        }
    }
    None
}

/// Run the three mechanically-detectable checks against `draft` text.
/// Returns every finding (a draft can violate more than one check at
/// once), in brand.md's own check order.
fn deterministic_scan(draft: &str) -> Vec<ScanFinding> {
    [
        scan_rhetorical_contrast(draft),
        scan_bullet_triplet(draft),
        scan_stacked_em_dashes(draft),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// Build the critic prompt covering the checks the deterministic scan
/// cannot safely make: hedge phrases, summary filler, and the read-aloud
/// test. Carries [`RUBRIC`] verbatim so the model sees the full six-check
/// bank even though it is being asked to judge only the remaining three.
fn build_prompt(draft: &str) -> String {
    format!(
        "You are critiquing a drafted LinkedIn post against this brand voice rubric. The \
         mechanical checks (rhetorical contrast setups, bold-label bullet triplets, stacked \
         em-dashes) have already been scanned for separately — judge only the remaining checks: \
         hedge phrases, summary filler, and the read-aloud test.\n\n\
         RUBRIC (verbatim from brand.md \"Voice — avoiding AI slop\"):\n{RUBRIC}\n\n\
         Respond with strict JSON matching {{\"verdict\": \"pass\" | \"revise\", \"confidence\": \
         number (0.0-1.0), \"issues\": [str]}} — \"issues\" should name the specific check violated \
         when the verdict is \"revise\", and may be empty on \"pass\".\n\n\
         Draft:\n{draft}"
    )
}

/// Read the draft text under critique from the bound `draft_input`
/// identity (falling back to its own prior pass first via
/// `revise::NODE_NAME` — mirrors `self_critic.rs`'s
/// `ReviseNode`-overwrites-first read preference — then the bound identity,
/// defaulting to [`draft::NODE_NAME`]). The resolved identity's stored
/// result must carry a top-level `"draft"` string field; task 6 (graph
/// assembly) is responsible for placing the candidate under review at that
/// shape before this node runs.
fn read_draft(ctx: &TaskContext, draft_input: &InputBinding) -> Result<String, NodeError> {
    let bound = draft_input.resolve(draft::NODE_NAME);
    let stored = get_result(ctx, revise::NODE_NAME)
        .or_else(|| get_result(ctx, bound))
        .ok_or_else(|| NodeError::new(format!("{NODE_NAME}: no draft stored by {bound}")))?;
    stored
        .get("draft")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| NodeError::new(format!("{NODE_NAME}: stored result missing `draft`")))
}

/// Read the current loop pass from `IncrementCriticIterationNode`'s stored
/// counter (bound via `iteration_input`, falling back to its default
/// identity when unbound), defaulting to 0 when absent. Reuses
/// `content_pipeline::increment_critic_iteration::NODE_NAME` directly per
/// tasks.json task 5's "reuse ... `IncrementCriticIterationNode` for the
/// bounded revise loop" — the counter node itself is policy-agnostic, so
/// it needs no linkedin_post-specific copy.
fn read_iteration(ctx: &TaskContext, iteration_input: &InputBinding) -> u32 {
    let bound = iteration_input.resolve(increment_critic_iteration::NODE_NAME);
    get_result(ctx, bound)
        .and_then(|value| value.get("iteration"))
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0) as u32
}

/// The `critic`-stage model node gating drafts against brand.md's anti-slop
/// bank.
pub struct BrandCriticNode {
    config: claude_code_rs::Config,
    transport: Option<ModelTransport>,
    draft_input: InputBinding,
    iteration_input: InputBinding,
}

impl BrandCriticNode {
    /// Construct with the critic-output `json_schema` set; `process`
    /// overwrites `model` per the resolved `critic`-stage tier. Both
    /// upstream bindings start unbound — falls back to
    /// [`draft::NODE_NAME`] / `IncrementCriticIterationNode`'s identity.
    #[must_use]
    pub fn new() -> Self {
        Self {
            config: claude_code_rs::Config {
                json_schema: Some(critic_output_json_schema()),
                ..claude_code_rs::Config::default()
            },
            transport: None,
            draft_input: InputBinding::default(),
            iteration_input: InputBinding::default(),
        }
    }

    /// Override the transport used by the composed `ClaudeCodeStep`. Tests
    /// use this to stub a real subprocess call with a canned `Outcome`, so
    /// the gated suite never spawns a real `claude`. Only reached when the
    /// deterministic scan finds nothing — a draft the scan already caught
    /// never calls the transport.
    #[must_use]
    pub fn with_transport(mut self, transport: ModelTransport) -> Self {
        self.transport = Some(transport);
        self
    }

    /// Bind the identity this node reads its current draft from. Unbound
    /// falls back to [`draft::NODE_NAME`] (today's default).
    #[must_use]
    pub fn with_draft_input_from(mut self, upstream: impl Into<String>) -> Self {
        self.draft_input = InputBinding::bound(upstream);
        self
    }

    /// Bind the identity this node reads its loop counter from. Unbound
    /// falls back to `IncrementCriticIterationNode`'s identity (today's
    /// default).
    #[must_use]
    pub fn with_iteration_input_from(mut self, upstream: impl Into<String>) -> Self {
        self.iteration_input = InputBinding::bound(upstream);
        self
    }
}

impl Default for BrandCriticNode {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Node for BrandCriticNode {
    async fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let draft = read_draft(&ctx, &self.draft_input)?;
        let iteration = read_iteration(&ctx, &self.iteration_input);
        let policy: LinkedInPostPolicy = crate::policy::resolved_policy_strict(&ctx)?;

        let scan_findings = deterministic_scan(&draft);

        let (evaluation, mut ctx) = if !scan_findings.is_empty() {
            // A mechanical violation was found — never even call the
            // model; the draft is caught on the deterministic layer alone.
            let evaluation = CriticEvaluation {
                verdict: CriticVerdict::Revise,
                confidence: 0.0,
                issues: scan_findings.into_iter().map(|f| f.issue).collect(),
                iteration,
            };
            (evaluation, ctx)
        } else {
            let mut config = self.config.clone();
            config = crate::policy::apply_model_tier(
                config,
                policy.model_tiers.critic,
                &policy.local.model,
            );
            let prompt = build_prompt(&draft);

            let mut step = ClaudeCodeStep::new(NODE_NAME, config, prompt);
            if let Some(transport) = self.transport.clone() {
                step = step.with_transport(move |config, prompt| (transport)(config, prompt));
            }

            let inner_ctx = step.process(ctx).await?;

            let content = inner_ctx
                .nodes
                .get(NODE_NAME)
                .and_then(|value| value.get("content"))
                .and_then(|value| value.as_str())
                .unwrap_or_default()
                .to_string();

            let parsed: CriticOutput = parse_structured_or_fenced(&inner_ctx, NODE_NAME, &content)
                .map_err(|err| {
                    NodeError::new(format!(
                        "{NODE_NAME}: failed to parse a CriticEvaluation from the model's reply: \
                         {err}"
                    ))
                })?;

            let verdict = verdict_from_model_text(&parsed.verdict);
            let evaluation = CriticEvaluation {
                verdict,
                confidence: parsed.confidence,
                issues: parsed.issues,
                iteration,
            };
            (evaluation, inner_ctx)
        };

        // This pass's 1-based ordinal — mirrors
        // `content_pipeline::critic_router`'s "Iteration semantics" doc
        // comment: `iteration` is 0-based and counts revisions completed
        // *before* the pass just evaluated.
        let passes_so_far = iteration.saturating_add(1);
        let capped = !matches!(evaluation.verdict, CriticVerdict::Pass)
            && passes_so_far >= policy.max_critic_iterations;

        let mut result = serde_json::to_value(&evaluation).map_err(|err| {
            NodeError::new(format!("failed to serialize CriticEvaluation: {err}"))
        })?;
        result["capped"] = json!(capped);
        put_result(&mut ctx, NODE_NAME, result);

        Ok(ctx)
    }

    fn name(&self) -> &str {
        NODE_NAME
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashMap};

    use claude_code_rs::parse::{ModelUsage as SdkModelUsage, Usage as SdkUsage};
    use claude_code_rs::{Config, Outcome};
    use futures::FutureExt;

    use super::super::policy::LinkedInPostPolicy;
    use super::*;

    fn ctx_with_draft(node_name: &str, draft: &str) -> TaskContext {
        let mut ctx = TaskContext {
            event: json!({}),
            nodes: HashMap::new(),
            metadata: json!({}),
            node_runs: HashMap::new(),
        };
        put_result(&mut ctx, node_name, json!({ "draft": draft }));
        ctx.nodes.insert(
            crate::policy::RESOLVED_POLICY_IDENTITY.to_string(),
            serde_json::to_value(LinkedInPostPolicy::default()).expect("policy serializes"),
        );
        ctx
    }

    fn stub_critic_json(verdict: &str, confidence: f64, issues: Vec<&str>) -> serde_json::Value {
        json!({ "verdict": verdict, "confidence": confidence, "issues": issues })
    }

    fn stub_transport(structured: serde_json::Value) -> ModelTransport {
        std::sync::Arc::new(move |_config: Config, _prompt: String| {
            let structured = structured.clone();
            async move {
                Ok(Outcome {
                    text: serde_json::to_string(&structured).unwrap(),
                    cost_usd: 0.01,
                    usage: SdkUsage {
                        input_tokens: 10,
                        output_tokens: 5,
                        cache_creation_input_tokens: 0,
                        cache_read_input_tokens: 0,
                    },
                    model_usage: BTreeMap::from([(
                        "claude-sonnet-4-5".to_string(),
                        SdkModelUsage {
                            input_tokens: 10,
                            output_tokens: 5,
                            cache_read_input_tokens: 0,
                            cache_creation_input_tokens: 0,
                            cost_usd: 0.01,
                        },
                    )]),
                    structured_output: Some(structured),
                    is_error: false,
                    api_error_status: None,
                })
            }
            .boxed()
        })
    }

    fn evaluation_of(ctx: &TaskContext) -> CriticEvaluation {
        serde_json::from_value(ctx.nodes[NODE_NAME].clone()).expect("valid CriticEvaluation")
    }

    // AC1: rhetorical contrast setup is caught, issue names the contrast setup.
    #[tokio::test]
    async fn ac1_rhetorical_contrast_setup_is_caught() {
        let node = BrandCriticNode::new();
        let ctx = ctx_with_draft(
            draft::NODE_NAME,
            "This isn't just a script, it's a system that runs unattended.",
        );

        let ctx = node.process(ctx).await.expect("process should succeed");
        let evaluation = evaluation_of(&ctx);

        assert_eq!(evaluation.verdict, CriticVerdict::Revise);
        assert!(
            evaluation
                .issues
                .iter()
                .any(|i| i.contains("contrast setup")),
            "issues should name the contrast setup, got: {:?}",
            evaluation.issues
        );
    }

    // AC2: three consecutive bold-bullet lines are caught, issue names the triplet.
    #[tokio::test]
    async fn ac2_bold_bullet_triplet_is_caught() {
        let node = BrandCriticNode::new();
        let draft = "\
- **Speed** — it ships fast\n\
- **Safety** — it never breaks prod\n\
- **Clarity** — it reads clean";
        let ctx = ctx_with_draft(draft::NODE_NAME, draft);

        let ctx = node.process(ctx).await.expect("process should succeed");
        let evaluation = evaluation_of(&ctx);

        assert_eq!(evaluation.verdict, CriticVerdict::Revise);
        assert!(
            evaluation
                .issues
                .iter()
                .any(|i| i.contains("bullet triplet")),
            "issues should name the bullet triplet, got: {:?}",
            evaluation.issues
        );
    }

    // AC3: three em-dash asides in one paragraph are caught.
    #[tokio::test]
    async fn ac3_stacked_em_dashes_are_caught() {
        let node = BrandCriticNode::new();
        let draft = "I built this — a small workflow engine — over a few weeks — and it holds up.";
        let ctx = ctx_with_draft(draft::NODE_NAME, draft);

        let ctx = node.process(ctx).await.expect("process should succeed");
        let evaluation = evaluation_of(&ctx);

        assert_eq!(evaluation.verdict, CriticVerdict::Revise);
        assert!(
            evaluation.issues.iter().any(|i| i.contains("em-dash")),
            "issues should name the stacked em-dashes, got: {:?}",
            evaluation.issues
        );
    }

    // AC4: a clean draft passes on the first pass, no revise iteration.
    #[tokio::test]
    async fn ac4_clean_draft_passes_on_first_pass() {
        let node = BrandCriticNode::new().with_transport(stub_transport(stub_critic_json(
            "pass",
            0.95,
            vec![],
        )));
        let ctx = ctx_with_draft(
            draft::NODE_NAME,
            "I shipped a small Rust workflow engine this week. It reads real commits and drafts \
             posts from them.",
        );

        let ctx = node.process(ctx).await.expect("process should succeed");
        let evaluation = evaluation_of(&ctx);

        assert_eq!(evaluation.verdict, CriticVerdict::Pass);
        assert_eq!(evaluation.iteration, 0);
        assert!(!ctx.nodes[NODE_NAME]["capped"].as_bool().unwrap());
    }

    // AC5: a draft that never passes within the cap is returned with an
    // explicit failing marker, not as a pass.
    #[tokio::test]
    async fn ac5_never_passing_draft_is_marked_capped_at_the_iteration_ceiling() {
        let policy = LinkedInPostPolicy {
            max_critic_iterations: 2,
            ..LinkedInPostPolicy::default()
        };
        let node = BrandCriticNode::new().with_transport(stub_transport(stub_critic_json(
            "revise",
            0.2,
            vec!["still off"],
        )));
        let mut ctx = ctx_with_draft(draft::NODE_NAME, "A merely mediocre draft with no tics.");
        ctx.nodes.insert(
            crate::policy::RESOLVED_POLICY_IDENTITY.to_string(),
            serde_json::to_value(&policy).expect("policy serializes"),
        );
        // Simulate this being the loop's terminal (2nd) pass.
        put_result(
            &mut ctx,
            increment_critic_iteration::NODE_NAME,
            json!({ "iteration": 1 }),
        );

        let ctx = node.process(ctx).await.expect("process should succeed");
        let evaluation = evaluation_of(&ctx);

        assert_eq!(evaluation.verdict, CriticVerdict::Revise);
        assert!(
            ctx.nodes[NODE_NAME]["capped"].as_bool().unwrap(),
            "a draft still failing at the cap must be marked capped, not silently passed"
        );
    }

    #[tokio::test]
    async fn deterministic_scan_short_circuits_without_calling_the_transport() {
        // No transport is configured at all; if the node tried to call
        // one, `ClaudeCodeStep`'s default (real subprocess) path would be
        // exercised and this test would hang/fail in CI. Reaching a
        // Revise verdict here proves the scan alone decided the outcome.
        let node = BrandCriticNode::new();
        let ctx = ctx_with_draft(
            draft::NODE_NAME,
            "That gap between what we say and what we ship is closing fast.",
        );

        let ctx = node.process(ctx).await.expect("process should succeed");
        let evaluation = evaluation_of(&ctx);
        assert_eq!(evaluation.verdict, CriticVerdict::Revise);
    }

    #[tokio::test]
    async fn ambiguous_model_verdict_normalizes_to_revise() {
        let node = BrandCriticNode::new().with_transport(stub_transport(stub_critic_json(
            "maybe good enough",
            0.5,
            vec![],
        )));
        let ctx = ctx_with_draft(draft::NODE_NAME, "A clean, plainly stated draft.");

        let ctx = node.process(ctx).await.expect("process should succeed");
        let evaluation = evaluation_of(&ctx);
        assert_eq!(evaluation.verdict, CriticVerdict::Revise);
    }

    #[tokio::test]
    async fn process_stamps_iteration_from_increment_counter_state() {
        let node = BrandCriticNode::new().with_transport(stub_transport(stub_critic_json(
            "pass",
            0.9,
            vec![],
        )));
        let mut ctx = ctx_with_draft(draft::NODE_NAME, "A clean, plainly stated draft.");
        put_result(
            &mut ctx,
            increment_critic_iteration::NODE_NAME,
            json!({ "iteration": 2 }),
        );

        let ctx = node.process(ctx).await.expect("process should succeed");
        let evaluation = evaluation_of(&ctx);
        assert_eq!(evaluation.iteration, 2);
    }

    #[tokio::test]
    async fn process_reads_revise_node_draft_on_a_later_loop_pass() {
        let node = BrandCriticNode::new().with_transport(stub_transport(stub_critic_json(
            "pass",
            0.95,
            vec![],
        )));
        let ctx = ctx_with_draft(revise::NODE_NAME, "A revised, plainly stated draft.");

        let ctx = node.process(ctx).await.expect("process should succeed");
        let evaluation = evaluation_of(&ctx);
        assert_eq!(evaluation.verdict, CriticVerdict::Pass);
    }

    #[tokio::test]
    async fn process_reads_bound_draft_and_iteration_inputs() {
        let node = BrandCriticNode::new()
            .with_transport(stub_transport(stub_critic_json("pass", 0.9, vec![])))
            .with_draft_input_from("CustomDraftNode")
            .with_iteration_input_from("CustomIncrementNode");
        let mut ctx = ctx_with_draft("CustomDraftNode", "Bound draft text.");
        put_result(&mut ctx, "CustomIncrementNode", json!({ "iteration": 1 }));

        let ctx = node.process(ctx).await.expect("process should succeed");
        let evaluation = evaluation_of(&ctx);
        assert_eq!(evaluation.iteration, 1);
        assert_eq!(evaluation.verdict, CriticVerdict::Pass);
    }

    #[tokio::test]
    async fn prompt_carries_the_rubric_verbatim() {
        let captured: std::sync::Arc<std::sync::Mutex<Option<String>>> =
            std::sync::Arc::new(std::sync::Mutex::new(None));
        let captured_clone = captured.clone();
        let response = stub_critic_json("pass", 0.9, vec![]);
        let response_clone = response.clone();
        let transport: ModelTransport = std::sync::Arc::new(move |_config, prompt| {
            *captured_clone.lock().unwrap() = Some(prompt);
            let response = response_clone.clone();
            async move {
                Ok(Outcome {
                    text: serde_json::to_string(&response).unwrap(),
                    cost_usd: 0.0,
                    usage: SdkUsage {
                        input_tokens: 1,
                        output_tokens: 1,
                        cache_creation_input_tokens: 0,
                        cache_read_input_tokens: 0,
                    },
                    model_usage: BTreeMap::new(),
                    structured_output: Some(response),
                    is_error: false,
                    api_error_status: None,
                })
            }
            .boxed()
        });

        let node = BrandCriticNode::new().with_transport(transport);
        let ctx = ctx_with_draft(draft::NODE_NAME, "A clean, plainly stated draft.");
        node.process(ctx).await.expect("process should succeed");

        let prompt = captured.lock().unwrap().take().expect("transport called");
        assert!(prompt.contains(RUBRIC));
        assert!(prompt.contains("Hedge phrases."));
        assert!(prompt.contains("Read-aloud test."));
    }

    #[tokio::test]
    async fn applies_critic_stage_model_tier() {
        let policy = LinkedInPostPolicy {
            model_tiers: super::super::policy::ModelTiers {
                critic: super::super::policy::ModelTier::Opus,
                ..LinkedInPostPolicy::default().model_tiers
            },
            ..LinkedInPostPolicy::default()
        };

        let captured: std::sync::Arc<std::sync::Mutex<Option<Config>>> =
            std::sync::Arc::new(std::sync::Mutex::new(None));
        let captured_clone = captured.clone();
        let response = stub_critic_json("pass", 0.9, vec![]);
        let response_clone = response.clone();
        let transport: ModelTransport = std::sync::Arc::new(move |config, _prompt| {
            *captured_clone.lock().unwrap() = Some(config);
            let response = response_clone.clone();
            async move {
                Ok(Outcome {
                    text: serde_json::to_string(&response).unwrap(),
                    cost_usd: 0.0,
                    usage: SdkUsage {
                        input_tokens: 1,
                        output_tokens: 1,
                        cache_creation_input_tokens: 0,
                        cache_read_input_tokens: 0,
                    },
                    model_usage: BTreeMap::new(),
                    structured_output: Some(response),
                    is_error: false,
                    api_error_status: None,
                })
            }
            .boxed()
        });

        let node = BrandCriticNode::new().with_transport(transport);
        let mut ctx = ctx_with_draft(draft::NODE_NAME, "A clean, plainly stated draft.");
        ctx.nodes.insert(
            crate::policy::RESOLVED_POLICY_IDENTITY.to_string(),
            serde_json::to_value(&policy).expect("policy serializes"),
        );
        node.process(ctx).await.expect("process should succeed");

        let config = captured.lock().unwrap().take().expect("transport called");
        assert_eq!(config.model.as_deref(), Some("claude-opus-4-8"));
    }

    #[tokio::test]
    async fn process_errors_when_no_upstream_draft_is_stored() {
        let node = BrandCriticNode::new();
        let mut ctx = TaskContext {
            event: json!({}),
            nodes: HashMap::new(),
            metadata: json!({}),
            node_runs: HashMap::new(),
        };
        ctx.nodes.insert(
            crate::policy::RESOLVED_POLICY_IDENTITY.to_string(),
            serde_json::to_value(LinkedInPostPolicy::default()).expect("policy serializes"),
        );

        let err = node.process(ctx).await.expect_err("should fail");
        assert!(err.message.contains("no draft stored by"));
    }
}
