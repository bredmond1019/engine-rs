//! The `event` schema for `workflow_type = "LINKEDIN_POST"`, plus the draft
//! candidate and work-source shapes every downstream node in this module
//! shares.
//!
//! Source of truth: `planning/EN.5.G/tasks.md` + `tasks.json` task 1.
//!
//! **Traceability is a type invariant, not a prompt instruction** — a
//! [`PostCandidate`] with an empty `sources` fails to deserialize, so the
//! block's "traceable to real commits" acceptance criterion cannot be
//! satisfied by a model that merely claims compliance in prose.

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};

fn default_candidate_count() -> u32 {
    3
}

/// The `event` schema for `workflow_type = "LINKEDIN_POST"`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkedInPostEventSchema {
    /// ISO-8601 date (e.g. `"2026-08-17"`) — start of the window to read
    /// real work from.
    pub since: String,
    /// ISO-8601 date — end of the window (inclusive).
    pub until: String,
    /// Repos to read from; defaults to the whole fleet when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repos: Option<Vec<String>>,
    /// How many post candidates to propose.
    #[serde(default = "default_candidate_count")]
    pub candidate_count: u32,
}

/// What kind of real-work artifact backs a [`PostCandidate`]'s claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WorkSourceKind {
    Commit,
    LogEntry,
    Decision,
}

/// One real-work artifact — a commit, a `log.md` entry, or a
/// `planning/decisions/` file — that a [`PostCandidate`] traces its claims
/// back to.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkSource {
    pub kind: WorkSourceKind,
    /// Stable identifier for the source (commit SHA, log entry date/slug,
    /// decision doc id).
    pub id: String,
    pub summary: String,
}

/// A shadow of [`PostCandidate`] used only to deserialize before the
/// non-empty-`sources` invariant is checked. Keeping this private and
/// field-identical means the public type never diverges from what actually
/// round-trips.
#[derive(Debug, Clone, Deserialize)]
struct PostCandidateShadow {
    angle: String,
    draft: String,
    sources: Vec<WorkSource>,
}

/// One drafted LinkedIn post candidate. `sources` is REQUIRED and
/// non-empty — enforced at the deserialization boundary, not merely by
/// convention, so a candidate that cannot point at real work cannot exist
/// as a value of this type.
#[derive(Debug, Clone, Serialize)]
pub struct PostCandidate {
    pub angle: String,
    pub draft: String,
    pub sources: Vec<WorkSource>,
}

impl<'de> Deserialize<'de> for PostCandidate {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let shadow = PostCandidateShadow::deserialize(deserializer)?;
        if shadow.sources.is_empty() {
            return Err(D::Error::custom(
                "PostCandidate.sources must be non-empty: every post candidate must trace to \
                 real commits, log entries, or decisions (traceability requirement)",
            ));
        }
        Ok(PostCandidate {
            angle: shadow.angle,
            draft: shadow.draft,
            sources: shadow.sources,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn minimal_event() -> serde_json::Value {
        json!({
            "since": "2026-08-17",
            "until": "2026-08-24",
        })
    }

    #[test]
    fn event_deserializes_range_only_and_defaults_candidate_count_to_3() {
        let input: LinkedInPostEventSchema =
            serde_json::from_value(minimal_event()).expect("deserializes");

        assert_eq!(input.since, "2026-08-17");
        assert_eq!(input.until, "2026-08-24");
        assert!(input.repos.is_none());
        assert_eq!(input.candidate_count, 3);
    }

    #[test]
    fn event_honors_explicit_repos_and_candidate_count() {
        let mut event = minimal_event();
        event["repos"] = json!(["engine-rs", "bastion"]);
        event["candidate_count"] = json!(5);

        let input: LinkedInPostEventSchema = serde_json::from_value(event).expect("deserializes");

        assert_eq!(
            input.repos,
            Some(vec!["engine-rs".to_string(), "bastion".to_string()])
        );
        assert_eq!(input.candidate_count, 5);
    }

    fn work_source(kind: WorkSourceKind) -> serde_json::Value {
        json!({ "kind": kind_str(kind), "id": "abc123", "summary": "did a thing" })
    }

    fn kind_str(kind: WorkSourceKind) -> &'static str {
        match kind {
            WorkSourceKind::Commit => "commit",
            WorkSourceKind::LogEntry => "log-entry",
            WorkSourceKind::Decision => "decision",
        }
    }

    #[test]
    fn work_source_kind_round_trips_kebab_case() {
        for kind in [
            WorkSourceKind::Commit,
            WorkSourceKind::LogEntry,
            WorkSourceKind::Decision,
        ] {
            let value = work_source(kind);
            let parsed: WorkSource = serde_json::from_value(value).expect("deserializes");
            assert_eq!(parsed.kind, kind);
        }
    }

    #[test]
    fn post_candidate_with_sources_deserializes() {
        let value = json!({
            "angle": "shipped a workflow engine",
            "draft": "This week I built...",
            "sources": [work_source(WorkSourceKind::Commit)],
        });

        let candidate: PostCandidate = serde_json::from_value(value).expect("deserializes");
        assert_eq!(candidate.sources.len(), 1);
    }

    #[test]
    fn post_candidate_with_empty_sources_is_rejected() {
        let value = json!({
            "angle": "shipped a workflow engine",
            "draft": "This week I built...",
            "sources": [],
        });

        let err = serde_json::from_value::<PostCandidate>(value)
            .expect_err("empty sources must be rejected");
        assert!(
            err.to_string().contains("traceability"),
            "error should name the traceability requirement, got: {err}"
        );
    }

    #[test]
    fn post_candidate_serializes_back_out() {
        let candidate = PostCandidate {
            angle: "angle".to_string(),
            draft: "draft".to_string(),
            sources: vec![WorkSource {
                kind: WorkSourceKind::LogEntry,
                id: "2026-08-24".to_string(),
                summary: "logged work".to_string(),
            }],
        };

        let value = serde_json::to_value(&candidate).expect("serializes");
        assert_eq!(value["sources"][0]["kind"], json!("log-entry"));
    }
}
