//! `ClaimReaffirmInput` (the triggering event schema), the per-claim state
//! types (`ClaimItem`/`ClaimStatus`/`Verdict`/`VerdictAction`/`Citation`/
//! `TransportInfo`), `ClaimReaffirmState`, and the `ClaimReaffirmPolicy`
//! four-layer policy (standing rule 6) — task 1 of `EN.6.L`.
//!
//! There is no separate `policy.rs`/`profiles.rs` module for this workflow
//! (unlike `research_agent`/`content_pipeline`) — no task in this spec's
//! `tasks.json` owns those files, and every later task (`queue_router.rs`,
//! `judge.rs`, `save_verdict.rs`) reads the policy shape defined here
//! without touching this file again. So the full knob set task 2 needs
//! (`max_attempts`, `judge_model_tier`, `recall_limit`) is defined now,
//! resolved through the standard four layers (per-run event `policy`
//! override > named `profile` bundle > `planning/harness.json`
//! `claim_reaffirm.policy` defaults > built-in default), with the three
//! canonical profile bundles (`baseline`/`cheap-fast`/`thorough`) all set —
//! mirroring `approve_and_run::policy`/`approve_and_run::profiles` merged
//! into one file.

use serde::{Deserialize, Serialize};

use crate::node::NodeError;
use crate::policy::{merge_opt, ModelTier, Policy, PolicyConfigSource};

/// The `harness.json` section key this workflow's policy/profiles live
/// under (`claim_reaffirm.policy` / `claim_reaffirm.profiles`).
const WORKFLOW_KEY: &str = "claim_reaffirm";

// ---------------------------------------------------------------------------
// Policy
// ---------------------------------------------------------------------------

/// The fully-resolved, per-run `CLAIM_REAFFIRM` policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimReaffirmPolicy {
    /// How many times `ClaimRecallNode`/`JudgeClaimNode` may retry a single
    /// claim (recall failure or a judge call that raises) before that claim
    /// is marked [`ClaimStatus::Failed`] and the drain moves on rather than
    /// halting the whole lane (task 2's per-item containment).
    pub max_attempts: u32,
    /// The `ClaudeCodeStep` model tier `JudgeClaimNode` (task 2) resolves
    /// for its one-verdict-per-claim call. A `Local`-tier run is subject to
    /// `openai_compat_transport.rs`'s silent local->cloud fallback — see
    /// [`super::TransportInfo`], which every `Verdict` stamps so that
    /// fallback stays visible per claim rather than silent.
    pub judge_model_tier: ModelTier,
    /// The `limit` passed to `ClaimRecallNode`'s underlying `RecallNode`
    /// query (task 2) — how many corpus evidence hits to fetch per claim.
    pub recall_limit: u32,
}

impl Default for ClaimReaffirmPolicy {
    /// Behavior-stable baseline: 3 attempts per claim, the judge runs on
    /// `Sonnet` (never defaults to `Local` — matches
    /// `research_agent::policy`'s reasoning: a fresh knob must not silently
    /// change what an existing/unconfigured run does, and `Sonnet` is this
    /// crate's existing tier default, `ModelTier::default()`), and up to 5
    /// recall hits per claim.
    fn default() -> Self {
        Self {
            max_attempts: 3,
            judge_model_tier: ModelTier::Sonnet,
            recall_limit: 5,
        }
    }
}

/// All-optional mirror of [`ClaimReaffirmPolicy`] used by the override
/// layers (`harness.json`'s `claim_reaffirm.policy`, a named `profile`, and
/// a per-run event's `policy` field). Every field left `None` falls through
/// to the next-lower-precedence layer.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PartialClaimReaffirmPolicy {
    pub max_attempts: Option<u32>,
    pub judge_model_tier: Option<ModelTier>,
    pub recall_limit: Option<u32>,
}

impl Policy for ClaimReaffirmPolicy {
    type Partial = PartialClaimReaffirmPolicy;

    /// Apply one override layer on top of `self`, field-by-field (`Some` in
    /// `over` wins, `None` falls through to `self`).
    fn apply(self, over: &PartialClaimReaffirmPolicy) -> Self {
        Self {
            max_attempts: merge_opt(self.max_attempts, over.max_attempts),
            judge_model_tier: merge_opt(self.judge_model_tier, over.judge_model_tier),
            recall_limit: merge_opt(self.recall_limit, over.recall_limit),
        }
    }
}

/// The explicit control profile: spelled out explicitly (rather than left
/// all-`None`) so selecting `profile: "baseline"` is a legible,
/// self-documenting no-op against the built-in default.
#[must_use]
pub fn baseline() -> PartialClaimReaffirmPolicy {
    PartialClaimReaffirmPolicy {
        max_attempts: Some(3),
        judge_model_tier: Some(ModelTier::Sonnet),
        recall_limit: Some(5),
    }
}

/// Cost/latency floor: fewer retries, a cheaper judge tier, fewer recall
/// hits per claim — a faster, cheaper pass over a large stale-claim wall.
#[must_use]
pub fn cheap_fast() -> PartialClaimReaffirmPolicy {
    PartialClaimReaffirmPolicy {
        max_attempts: Some(1),
        judge_model_tier: Some(ModelTier::Haiku),
        recall_limit: Some(3),
    }
}

/// Quality ceiling: more retries before giving up on a claim, the strongest
/// judge tier, and more recall evidence considered per claim.
#[must_use]
pub fn thorough() -> PartialClaimReaffirmPolicy {
    PartialClaimReaffirmPolicy {
        max_attempts: Some(5),
        judge_model_tier: Some(ModelTier::Opus),
        recall_limit: Some(10),
    }
}

/// Resolve a built-in profile bundle by its kebab-case name. Returns `None`
/// for any name that isn't one of the three canonical profiles.
#[must_use]
pub fn profile_by_name(name: &str) -> Option<PartialClaimReaffirmPolicy> {
    match name {
        "baseline" => Some(baseline()),
        "cheap-fast" => Some(cheap_fast()),
        "thorough" => Some(thorough()),
        _ => None,
    }
}

/// Read `claim_reaffirm.policy` (a [`PartialClaimReaffirmPolicy`]) out of
/// the file addressed by `source`. Delegates to the generic
/// `crate::policy::read_harness_policy_defaults_from`, parameterized by
/// [`WORKFLOW_KEY`].
pub fn read_harness_policy_defaults_from(
    source: &PolicyConfigSource,
) -> Result<Option<PartialClaimReaffirmPolicy>, NodeError> {
    crate::policy::read_harness_policy_defaults_from(source, WORKFLOW_KEY)
}

/// Resolve a named `profile` to a [`PartialClaimReaffirmPolicy`] bundle,
/// preferring a `harness.json` `claim_reaffirm.profiles[name]` entry (read
/// via `source`) over the built-in [`profile_by_name`]. Returns `Ok(None)`
/// when `profile_name` is `None`, and `Err` when a name is given but
/// resolves to neither source (no silent no-op).
pub fn resolve_profile_from(
    profile_name: Option<&str>,
    source: &PolicyConfigSource,
) -> Result<Option<PartialClaimReaffirmPolicy>, NodeError> {
    crate::policy::resolve_profile_from(profile_name, source, WORKFLOW_KEY, profile_by_name)
}

/// Resolve the four-layer [`ClaimReaffirmPolicy`] for a run: `event_override`
/// (a per-run `policy` field), the resolved `profile_name` bundle, `source`'s
/// `claim_reaffirm.policy` defaults, and the built-in default, high->low
/// precedence via `crate::policy::resolve`.
pub fn resolve_policy_for_run_from(
    source: &PolicyConfigSource,
    profile_name: Option<&str>,
    event_override: Option<&PartialClaimReaffirmPolicy>,
) -> Result<ClaimReaffirmPolicy, NodeError> {
    let harness_defaults = read_harness_policy_defaults_from(source)?;
    let profile = resolve_profile_from(profile_name, source)?;
    Ok(crate::policy::resolve(
        ClaimReaffirmPolicy::default(),
        harness_defaults.as_ref(),
        profile.as_ref(),
        event_override,
    ))
}

// ---------------------------------------------------------------------------
// Event schema
// ---------------------------------------------------------------------------

/// Inbound event schema for the `CLAIM_REAFFIRM` workflow. Mirrors the
/// `policy`/`profile` override-layer fields of `SDLCFlowEventSchema`/
/// `ResearchAgentEventSchema`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ClaimReaffirmInput {
    /// Explicit claim lane, bypassing the fleet-wide `knowledge.md`/
    /// `memory.md` scan entirely. Set by tests (and by an operator wanting
    /// to re-run a specific, already-known subset) rather than trusting
    /// `LoadClaimsNode`'s own corpus discovery. `None` is the production
    /// path: `LoadClaimsNode` resolves the brain root and scans every
    /// registered repo's distilled files.
    #[serde(default)]
    pub lane_source_override: Option<Vec<ClaimItem>>,
    /// Optional per-run policy override — the highest-precedence of the
    /// four [`ClaimReaffirmPolicy`] resolution layers (event override >
    /// named `profile` > `harness.json` defaults > built-in default).
    #[serde(default)]
    pub policy: Option<PartialClaimReaffirmPolicy>,
    /// Optional name of a built-in or `harness.json`-defined policy profile
    /// bundle (e.g. `"baseline"`) to apply for this run.
    #[serde(default)]
    pub profile: Option<String>,
}

// ---------------------------------------------------------------------------
// Per-claim state
// ---------------------------------------------------------------------------

/// Where one claim sits in the queue-drain loop (task 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimStatus {
    /// Not yet judged — still in the drain queue.
    Pending,
    /// A [`Verdict`] has been recorded for this claim.
    Judged,
    /// Recall or judgment failed `max_attempts` times in a row; the drain
    /// gave up on this claim rather than halting the whole lane (task 2's
    /// per-item containment).
    Failed,
}

/// The reviewable action a claim's verdict proposes. The engine only ever
/// proposes — mev writes, a human approves (OR.K3 constraint, inherited).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerdictAction {
    /// Bump the entry's `freshness:` stamp — the claim still holds and the
    /// corpus evidence supports it. Never chosen when recall returned no
    /// evidence (OR.K3, enforced structurally in `JudgeClaimNode`, task 2).
    BumpFreshness,
    /// The claim has been superseded by newer corpus content.
    Supersede,
    /// The claim should be archived — no longer worth keeping fresh.
    Archive,
    /// The judge could not reach a confident verdict; route to a human.
    NeedsHuman,
}

/// One piece of corpus evidence a `Verdict` cites — a recall hit's doc
/// identity plus the snippet that justified the verdict.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Citation {
    /// The corpus document identity the evidence came from (a `doc_id` or,
    /// failing that, a `file_path` — whichever the recall result carried).
    pub doc_id: String,
    /// The evidence source's file path, when the recall result carried one.
    #[serde(default)]
    pub file_path: Option<String>,
    /// The cited snippet itself.
    #[serde(default)]
    pub snippet: String,
}

/// Which transport actually served a `JudgeClaimNode` call — mirrors the
/// `"transport"` object `ClaudeCodeStep` stamps onto `ctx.nodes[name]`
/// (`tier`/`model`/`endpoint`), carried onto the `Verdict` itself so the
/// silent local->cloud fallback (`openai_compat_transport.rs:18-24`) stays
/// visible per claim rather than only in run-level telemetry.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransportInfo {
    /// `"cloud"` or `"local"`.
    pub tier: String,
    /// The concrete model/endpoint name that actually served the call.
    pub model: String,
    /// The local endpoint URL, when `tier == "local"`; `None` for a cloud
    /// call.
    #[serde(default)]
    pub endpoint: Option<String>,
}

/// The verdict `JudgeClaimNode` (task 2) records for one claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Verdict {
    pub action: VerdictAction,
    /// Corpus evidence supporting `action`. An empty set is only ever
    /// paired with [`VerdictAction::Archive`] or
    /// [`VerdictAction::NeedsHuman`] (OR.K3) — a `BumpFreshness`/
    /// `Supersede` verdict always carries at least one citation.
    #[serde(default)]
    pub evidence: Vec<Citation>,
    /// The judge's stated reasoning.
    pub reasoning: String,
    /// The transport that served the judge call, when known.
    #[serde(default)]
    pub transport: Option<TransportInfo>,
}

/// One claim moving through the queue-drain loop, from lane ingestion
/// through to a recorded verdict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimItem {
    /// Stable identity for this claim within the run — a hash of
    /// `source_doc_id` + the claim text's provenance line, so a re-trigger
    /// against the same corpus reproduces the same ids (task 1's own
    /// [`load_id`] helper).
    pub id: String,
    /// The corpus document this claim's `source:` field names — the
    /// identifier [`super::load_claims`]'s `RecallNode` query (task 2)
    /// anchors on ("the identifier-anchored pattern the Brain's own
    /// knowledge.md documents as what scores").
    pub source_doc_id: String,
    /// The claim's full bold-claim text, as authored in `knowledge.md`/
    /// `memory.md`.
    pub claim_text: String,
    /// The claim's D35 `freshness:` stamp (`YYYY-MM-DD`), as authored.
    /// `None` when the source entry's `freshness:` field did not parse —
    /// mirrors `mev::brain::distill::DistilledEntry::freshness`.
    #[serde(default)]
    pub freshness_date: Option<String>,
    pub status: ClaimStatus,
    /// How many recall/judge attempts have been made on this claim so far,
    /// bounded by `ClaimReaffirmPolicy::max_attempts`.
    #[serde(default)]
    pub attempt: u32,
    #[serde(default)]
    pub verdict: Option<Verdict>,
}

/// Workflow-level state: the whole claims array, mutated in place
/// (read-modify-write, the `SDLCState` precedent — `put_result` overwrites
/// the identity wholesale) across every pass of the queue-drain loop.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimReaffirmState {
    pub claims: Vec<ClaimItem>,
}

impl ClaimReaffirmState {
    /// `true` once every claim has left [`ClaimStatus::Pending`] — the
    /// drain-exit condition `ClaimQueueRouterNode` (task 2) checks.
    #[must_use]
    pub fn is_drained(&self) -> bool {
        self.claims
            .iter()
            .all(|c| !matches!(c.status, ClaimStatus::Pending))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- Policy -------------------------------------------------------

    #[test]
    fn builtin_default_is_behavior_stable_baseline() {
        let policy = ClaimReaffirmPolicy::default();
        assert_eq!(policy.max_attempts, 3);
        assert_eq!(policy.judge_model_tier, ModelTier::Sonnet);
        assert_eq!(policy.recall_limit, 5);
    }

    #[test]
    fn resolve_with_no_overrides_returns_builtin() {
        let resolved = crate::policy::resolve(ClaimReaffirmPolicy::default(), None, None, None);
        assert_eq!(resolved, ClaimReaffirmPolicy::default());
    }

    #[test]
    fn event_override_beats_profile_beats_harness_defaults() {
        let harness = PartialClaimReaffirmPolicy {
            recall_limit: Some(1),
            ..Default::default()
        };
        let profile = PartialClaimReaffirmPolicy {
            recall_limit: Some(2),
            max_attempts: Some(9),
            ..Default::default()
        };
        let event = PartialClaimReaffirmPolicy {
            recall_limit: Some(3),
            ..Default::default()
        };
        let resolved = crate::policy::resolve(
            ClaimReaffirmPolicy::default(),
            Some(&harness),
            Some(&profile),
            Some(&event),
        );
        assert_eq!(
            resolved.recall_limit, 3,
            "event beats profile beats harness"
        );
        assert_eq!(
            resolved.max_attempts, 9,
            "untouched-by-event knob falls through to profile"
        );
        assert_eq!(
            resolved.judge_model_tier,
            ModelTier::Sonnet,
            "untouched-by-any-override knob falls through to builtin"
        );
    }

    #[test]
    fn profile_by_name_resolves_all_three_canonical_names() {
        assert!(profile_by_name("baseline").is_some());
        assert!(profile_by_name("cheap-fast").is_some());
        assert!(profile_by_name("thorough").is_some());
        assert!(profile_by_name("nonexistent").is_none());
    }

    #[test]
    fn every_profile_sets_every_knob() {
        for profile in [baseline(), cheap_fast(), thorough()] {
            assert!(profile.max_attempts.is_some());
            assert!(profile.judge_model_tier.is_some());
            assert!(profile.recall_limit.is_some());
        }
    }

    #[test]
    fn deserializes_partial_policy_from_harness_json_shape() {
        let json = r#"{
            "max_attempts": 4,
            "judge_model_tier": "haiku",
            "recall_limit": 7
        }"#;
        let partial: PartialClaimReaffirmPolicy =
            serde_json::from_str(json).expect("valid PartialClaimReaffirmPolicy JSON");
        assert_eq!(partial.max_attempts, Some(4));
        assert_eq!(partial.judge_model_tier, Some(ModelTier::Haiku));
        assert_eq!(partial.recall_limit, Some(7));
    }

    // -- Event schema ---------------------------------------------------

    #[test]
    fn event_schema_round_trips_with_lane_source_override() {
        let input = ClaimReaffirmInput {
            lane_source_override: Some(vec![ClaimItem {
                id: "abc123".to_string(),
                source_doc_id: "engine-rs/planning/knowledge.md".to_string(),
                claim_text: "some claim".to_string(),
                freshness_date: Some("2026-01-01".to_string()),
                status: ClaimStatus::Pending,
                attempt: 0,
                verdict: None,
            }]),
            policy: Some(PartialClaimReaffirmPolicy {
                max_attempts: Some(1),
                ..Default::default()
            }),
            profile: Some("baseline".to_string()),
        };
        let json = serde_json::to_string(&input).expect("serializes");
        let round_tripped: ClaimReaffirmInput = serde_json::from_str(&json).expect("deserializes");
        assert_eq!(round_tripped, input);
    }

    #[test]
    fn event_schema_defaults_are_all_none() {
        let input: ClaimReaffirmInput =
            serde_json::from_str("{}").expect("empty object deserializes via #[serde(default)]");
        assert_eq!(input.lane_source_override, None);
        assert_eq!(input.policy, None);
        assert_eq!(input.profile, None);
    }

    // -- Per-claim state --------------------------------------------------

    #[test]
    fn claim_item_serde_round_trip() {
        let item = ClaimItem {
            id: "abc123".to_string(),
            source_doc_id: "engine-rs/planning/knowledge.md".to_string(),
            claim_text: "some claim text".to_string(),
            freshness_date: Some("2026-01-01".to_string()),
            status: ClaimStatus::Judged,
            attempt: 1,
            verdict: Some(Verdict {
                action: VerdictAction::BumpFreshness,
                evidence: vec![Citation {
                    doc_id: "engine-rs/planning/status.md".to_string(),
                    file_path: Some("planning/status.md".to_string()),
                    snippet: "still true".to_string(),
                }],
                reasoning: "corroborated by status.md".to_string(),
                transport: Some(TransportInfo {
                    tier: "cloud".to_string(),
                    model: "claude-sonnet-4-5".to_string(),
                    endpoint: None,
                }),
            }),
        };
        let json = serde_json::to_string(&item).expect("serializes");
        let round_tripped: ClaimItem = serde_json::from_str(&json).expect("deserializes");
        assert_eq!(round_tripped, item);
    }

    #[test]
    fn claim_reaffirm_state_serde_round_trip() {
        let state = ClaimReaffirmState {
            claims: vec![
                ClaimItem {
                    id: "a".to_string(),
                    source_doc_id: "repo/planning/knowledge.md".to_string(),
                    claim_text: "claim a".to_string(),
                    freshness_date: None,
                    status: ClaimStatus::Pending,
                    attempt: 0,
                    verdict: None,
                },
                ClaimItem {
                    id: "b".to_string(),
                    source_doc_id: "repo/planning/memory.md".to_string(),
                    claim_text: "claim b".to_string(),
                    freshness_date: Some("2026-01-01".to_string()),
                    status: ClaimStatus::Failed,
                    attempt: 3,
                    verdict: None,
                },
            ],
        };
        let json = serde_json::to_string(&state).expect("serializes");
        let round_tripped: ClaimReaffirmState = serde_json::from_str(&json).expect("deserializes");
        assert_eq!(round_tripped, state);
    }

    #[test]
    fn is_drained_true_when_no_pending_claims_remain() {
        let mut state = ClaimReaffirmState::default();
        assert!(state.is_drained(), "an empty lane is trivially drained");

        state.claims.push(ClaimItem {
            id: "a".to_string(),
            source_doc_id: "repo/planning/knowledge.md".to_string(),
            claim_text: "claim a".to_string(),
            freshness_date: None,
            status: ClaimStatus::Pending,
            attempt: 0,
            verdict: None,
        });
        assert!(!state.is_drained());

        state.claims[0].status = ClaimStatus::Judged;
        assert!(state.is_drained());
    }
}
