//! The `event` schema for `workflow_type = "CONTENT_PIPELINE"`, plus the
//! self-critic loop's evaluation shape and the terminal output shape.
//!
//! Source of truth: `planning/EN.5.A-content-pipeline/architecture.md` §3.2.

use engine_contract::envelope::{ChannelType, IngressEnvelope};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::policy::PartialContentPipelinePolicy;

fn default_target_lang() -> String {
    "pt-BR".to_string()
}

/// An optional chain/trigger request an inbound `CONTENT_PIPELINE` event can
/// carry, asking `ActionDispatchNode` (`EN.6.A` task 3) to dispatch a
/// follow-on `TriggerWorkflow` action once this run completes. Mirrors the
/// private shape `action_dispatch.rs` already parses directly off the raw
/// event — this is the typed schema surface for the same field (task 3
/// predates this typed field on `ContentPipelineInput`; task 4 adds it here
/// so the declared schema documents what the node already accepts).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TriggerRequest {
    pub workflow_type: String,
    #[serde(default)]
    pub event: Value,
}

/// The `event` schema for `workflow_type = "CONTENT_PIPELINE"`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentPipelineInput {
    pub envelope: IngressEnvelope,
    #[serde(default)]
    pub translate: bool,
    #[serde(default = "default_target_lang")]
    pub target_lang: String,
    /// Per-event policy override (EN.4.0 convention).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<PartialContentPipelinePolicy>,
    /// Named profile override (EN.4.0 convention).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// Optional workflow-chain request; see [`TriggerRequest`] (`EN.6.A`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger: Option<TriggerRequest>,
}

/// The self-critic verdict for one loop pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CriticVerdict {
    Pass,
    Revise,
}

/// What `SelfCriticNode` emits each loop pass and `CriticRouterNode` guards
/// on. The `SelfCriticNode` `put_result` slot is overwritten each loop
/// pass — per-iteration history is carried by `iteration`, not `node_runs`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CriticEvaluation {
    pub verdict: CriticVerdict,
    /// 0.0-1.0; compared against `critic_confidence_threshold`.
    pub confidence: f64,
    pub issues: Vec<String>,
    /// Loop pass this evaluation belongs to (0-based; stamped from
    /// `IncrementCriticIterationNode`'s counter — 0 when absent).
    pub iteration: u32,
}

/// What `DigestRenderNode` assembles and `PersistToBrainNode` ships.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentPipelineOutput {
    pub artifact_id: String,
    pub source_channel: ChannelType,
    pub summary: String,
    pub entities: Vec<String>,
    pub digest_markdown: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest_html: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub translated_markdown: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn minimal_web_article_event() -> serde_json::Value {
        json!({
            "envelope": {
                "envelope_id": "env-1",
                "channel_type": "web_article",
                "timestamp": "2026-07-25T00:00:00Z",
                "source": { "kind": "url", "url": "https://example.com/a" }
            }
        })
    }

    #[test]
    fn content_pipeline_input_deserializes_with_defaults_applied() {
        let input: ContentPipelineInput =
            serde_json::from_value(minimal_web_article_event()).expect("deserializes");

        assert!(!input.translate);
        assert_eq!(input.target_lang, "pt-BR");
        assert!(input.policy.is_none());
        assert!(input.profile.is_none());
        assert_eq!(input.envelope.envelope_id, "env-1");
        assert_eq!(input.envelope.channel_type, ChannelType::WebArticle);
    }

    #[test]
    fn content_pipeline_input_honors_explicit_overrides() {
        let mut event = minimal_web_article_event();
        event["translate"] = json!(true);
        event["target_lang"] = json!("es");
        event["profile"] = json!("baseline");

        let input: ContentPipelineInput = serde_json::from_value(event).expect("deserializes");

        assert!(input.translate);
        assert_eq!(input.target_lang, "es");
        assert_eq!(input.profile.as_deref(), Some("baseline"));
    }

    #[test]
    fn content_pipeline_input_deserializes_without_a_trigger_field() {
        let input: ContentPipelineInput =
            serde_json::from_value(minimal_web_article_event()).expect("deserializes");
        assert!(input.trigger.is_none());
    }

    #[test]
    fn content_pipeline_input_deserializes_with_a_trigger_field() {
        let mut event = minimal_web_article_event();
        event["trigger"] = json!({
            "workflow_type": "OTHER_WORKFLOW",
            "event": { "foo": "bar" },
        });

        let input: ContentPipelineInput = serde_json::from_value(event).expect("deserializes");
        let trigger = input.trigger.expect("trigger present");
        assert_eq!(trigger.workflow_type, "OTHER_WORKFLOW");
        assert_eq!(trigger.event["foo"], json!("bar"));
    }

    #[test]
    fn trigger_request_round_trips() {
        let trigger = TriggerRequest {
            workflow_type: "OTHER_WORKFLOW".to_string(),
            event: json!({ "foo": "bar" }),
        };
        let value = serde_json::to_value(&trigger).expect("serializes");
        let round_tripped: TriggerRequest = serde_json::from_value(value).expect("deserializes");
        assert_eq!(round_tripped, trigger);
    }

    #[test]
    fn critic_evaluation_round_trips() {
        let evaluation = CriticEvaluation {
            verdict: CriticVerdict::Revise,
            confidence: 0.42,
            issues: vec!["missing citation".to_string()],
            iteration: 2,
        };

        let value = serde_json::to_value(&evaluation).expect("serializes");
        let round_tripped: CriticEvaluation = serde_json::from_value(value).expect("deserializes");

        assert_eq!(round_tripped.verdict, evaluation.verdict);
        assert_eq!(round_tripped.confidence, evaluation.confidence);
        assert_eq!(round_tripped.issues, evaluation.issues);
        assert_eq!(round_tripped.iteration, evaluation.iteration);
    }

    #[test]
    fn content_pipeline_output_round_trips_without_optional_fields() {
        let output = ContentPipelineOutput {
            artifact_id: "artifact-1".to_string(),
            source_channel: ChannelType::YouTubeTranscript,
            summary: "summary".to_string(),
            entities: vec!["entity".to_string()],
            digest_markdown: "# digest".to_string(),
            digest_html: None,
            translated_markdown: None,
        };

        let value = serde_json::to_value(&output).expect("serializes");
        assert!(value.get("digest_html").is_none());
        assert!(value.get("translated_markdown").is_none());

        let round_tripped: ContentPipelineOutput =
            serde_json::from_value(value).expect("deserializes");
        assert_eq!(round_tripped.artifact_id, output.artifact_id);
        assert_eq!(round_tripped.source_channel, output.source_channel);
    }
}
