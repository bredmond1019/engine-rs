//! Compose a schema-valid `escalations.jsonl` line (`EN.15.G` Task 1).
//!
//! Per `base-template/scripts/escalation.schema.json` and the eleven-field
//! contract it and `check_escalations.py` enforce, an escalation written by
//! a Rust chain must carry all eleven required fields
//! (`ts_utc, repo, lane, kind, severity, channel, gate_id, summary,
//! verified_by, durable_home, verified_at_sha`), reject a prose
//! `verified_by` at *composition* time — not merely when the checker later
//! runs over the file — and make a four-`option` record structurally
//! impossible to build, not merely rejected at runtime.
//!
//! Read `.claude/workflows/finding-discipline.md` before touching this
//! module: the eleven fields (`verified_by` chief among them) are the
//! evidence discipline this file exists to enforce, not paperwork to
//! satisfy. "A bare adjective is the failure mode" — the same rule that
//! file states for a written finding is exactly what [`VerifiedBy::new`]
//! rejects here.
//!
//! [`EscalationRecord`] is the composed, always-schema-valid record;
//! [`EscalationRecord::to_json`] renders it to the exact JSON object shape
//! `check_escalations.py` validates, and [`EscalationRecord::to_jsonl_line`]
//! appends the trailing newline an append-only `.jsonl` file needs. Nothing
//! here writes to disk — that is `integrate.rs`'s job (`EN.15.G` Task 2),
//! which calls this module on the bail and hold paths.

use std::fmt;

use regex::Regex;
use serde_json::{json, Map, Value};

/// Why an escalation exists — mirrors `escalation.schema.json`'s `kind`
/// enum, which maps four of its six values onto
/// `.claude/workflows/begin-orchestration.md` Rule 6's "what you still must
/// not decide alone" cases (`operator-gate`, `bail`, `disagreement`,
/// `cross-repo-edit`), plus two that are not drawn from that list
/// (`interrupt-request`, `finding`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EscalationKind {
    OperatorGate,
    Bail,
    Disagreement,
    CrossRepoEdit,
    InterruptRequest,
    Finding,
}

impl EscalationKind {
    /// The wire value this kind serializes to, verbatim against the
    /// schema's `enum`.
    #[must_use]
    pub fn as_wire_str(self) -> &'static str {
        match self {
            EscalationKind::OperatorGate => "operator-gate",
            EscalationKind::Bail => "bail",
            EscalationKind::Disagreement => "disagreement",
            EscalationKind::CrossRepoEdit => "cross-repo-edit",
            EscalationKind::InterruptRequest => "interrupt-request",
            EscalationKind::Finding => "finding",
        }
    }
}

impl fmt::Display for EscalationKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_wire_str())
    }
}

/// How severe an escalation is — mirrors `escalation.schema.json`'s
/// `severity` enum. There is no third value; a gate that cannot decide
/// between the two is a `disagreement` `kind`, not a severity value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EscalationSeverity {
    Blocking,
    Advisory,
}

impl EscalationSeverity {
    #[must_use]
    pub fn as_wire_str(self) -> &'static str {
        match self {
            EscalationSeverity::Blocking => "blocking",
            EscalationSeverity::Advisory => "advisory",
        }
    }
}

impl fmt::Display for EscalationSeverity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_wire_str())
    }
}

/// One named response option offered to the operator on a `notification`
/// channel — a `{"key": ..., "label": ...}` pair. `label` is capped at the
/// schema's `OPTION_LABEL_MAX_CHARS` (20, the WhatsApp reply-button title
/// limit — `crate::operator::limits::WHATSAPP_MAX_BUTTON_LABEL_CHARS`) and
/// that cap is enforced here, at construction, not merely by the schema
/// later.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EscalationOption {
    pub key: String,
    pub label: String,
}

/// Maximum characters an [`EscalationOption`] label may carry — mirrors
/// `escalation.schema.json`'s `options.items.properties.label.maxLength`
/// and `crate::operator::limits::WHATSAPP_MAX_BUTTON_LABEL_CHARS`.
pub const OPTION_LABEL_MAX_CHARS: usize = 20;

impl EscalationOption {
    /// Construct a response option, rejecting an empty key/label or a label
    /// over [`OPTION_LABEL_MAX_CHARS`] at construction time.
    pub fn new(key: impl Into<String>, label: impl Into<String>) -> Result<Self, EscalationError> {
        let key = key.into();
        let label = label.into();
        if key.is_empty() {
            return Err(EscalationError::EmptyField("options[].key"));
        }
        if label.is_empty() {
            return Err(EscalationError::EmptyField("options[].label"));
        }
        if label.chars().count() > OPTION_LABEL_MAX_CHARS {
            return Err(EscalationError::OptionLabelTooLong {
                key,
                chars: label.chars().count(),
                max: OPTION_LABEL_MAX_CHARS,
            });
        }
        Ok(Self { key, label })
    }

    fn to_json(&self) -> Value {
        json!({"key": self.key, "label": self.label})
    }
}

/// The `notification` channel's response options — bounded to exactly two
/// or three entries **at the type level**. There is no variant that can
/// hold a fourth option: a caller cannot reach one no matter how it builds
/// this value, which is the acceptance criterion `EN.15.G` Task 1 names —
/// "a four-option record cannot be CONSTRUCTED — the bound is in the type,
/// not a runtime check." [`EscalationOptions::new`] is a *fallible*
/// entry point from an arbitrary-length `Vec` (a caller might have 0, 1, 4,
/// or 10 candidate options in hand); once past it, the value in hand can
/// never smuggle a fourth option back in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EscalationOptions {
    Two([EscalationOption; 2]),
    Three([EscalationOption; 3]),
}

impl EscalationOptions {
    /// Build from an already-validated pair.
    #[must_use]
    pub fn pair(a: EscalationOption, b: EscalationOption) -> Self {
        Self::Two([a, b])
    }

    /// Build from an already-validated triple.
    #[must_use]
    pub fn triple(a: EscalationOption, b: EscalationOption, c: EscalationOption) -> Self {
        Self::Three([a, b, c])
    }

    /// Fallible constructor from a `Vec` of arbitrary length — the seam a
    /// caller with a dynamically-built option list goes through. Anything
    /// other than exactly 2 or 3 entries is refused here; there is no
    /// [`EscalationOptions`] value a caller could hold afterwards that
    /// carries a fourth option.
    pub fn new(options: Vec<EscalationOption>) -> Result<Self, EscalationError> {
        match <[EscalationOption; 2]>::try_from(options) {
            Ok(pair) => Ok(Self::Two(pair)),
            Err(options) => match <[EscalationOption; 3]>::try_from(options) {
                Ok(triple) => Ok(Self::Three(triple)),
                Err(options) => Err(EscalationError::WrongOptionCount {
                    count: options.len(),
                }),
            },
        }
    }

    #[must_use]
    pub fn as_slice(&self) -> &[EscalationOption] {
        match self {
            EscalationOptions::Two(pair) => pair.as_slice(),
            EscalationOptions::Three(triple) => triple.as_slice(),
        }
    }

    fn to_json(&self) -> Value {
        Value::Array(
            self.as_slice()
                .iter()
                .map(EscalationOption::to_json)
                .collect(),
        )
    }
}

/// Which channel an escalation routes to — mirrors
/// `escalation.schema.json`'s `channel` string pattern
/// (`^(notification|session:.+)$`) plus its `if`/`then`/`else`
/// options requirement. Encoding this as an enum makes the schema's
/// conditional structurally true rather than merely checked: a
/// [`EscalationChannel::Notification`] always carries its
/// [`EscalationOptions`] (2 or 3 entries — see that type's own docs), and a
/// [`EscalationChannel::Session`] never carries any `options` field at all,
/// matching `core/engine-rs/crates/engine-core/src/operator/channel.rs`'s
/// Invariant 2: "declared at gate-definition time, never degraded."
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EscalationChannel {
    Notification(EscalationOptions),
    /// `session:<slug>` — the slug is stored bare (without the
    /// `session:` prefix); [`EscalationChannel::wire_value`] adds it back.
    Session(String),
}

impl EscalationChannel {
    /// Build a `notification` channel from a fallible option list.
    pub fn notification(options: Vec<EscalationOption>) -> Result<Self, EscalationError> {
        Ok(Self::Notification(EscalationOptions::new(options)?))
    }

    /// Build a `session:<slug>` channel, rejecting an empty slug.
    pub fn session(slug: impl Into<String>) -> Result<Self, EscalationError> {
        let slug = slug.into();
        if slug.is_empty() {
            return Err(EscalationError::EmptyField("channel session slug"));
        }
        Ok(Self::Session(slug))
    }

    /// The exact string this channel serializes to in the `channel` field,
    /// matching the schema's `^(notification|session:.+)$` pattern.
    #[must_use]
    pub fn wire_value(&self) -> String {
        match self {
            EscalationChannel::Notification(_) => "notification".to_string(),
            EscalationChannel::Session(slug) => format!("session:{slug}"),
        }
    }

    /// The options this channel carries, or `None` for a `session` channel
    /// — never both `Some` and empty/oversized, per [`EscalationOptions`].
    #[must_use]
    pub fn options(&self) -> Option<&EscalationOptions> {
        match self {
            EscalationChannel::Notification(options) => Some(options),
            EscalationChannel::Session(_) => None,
        }
    }
}

/// The evidence a claim in an escalation rests on — same contract as
/// ping-agent's field of the same name and `escalation.schema.json`'s
/// `verified_by` pattern: either a command line followed by its real
/// output on a later line, or the literal form `UNVERIFIED: <who claimed
/// it>`. **A bare adjective (`"measured"`, `"confirmed"`) satisfies
/// neither shape and is refused here, at composition time** — this is the
/// evidence discipline `.claude/workflows/finding-discipline.md` Rule 1
/// names, made structural rather than a review comment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifiedBy {
    /// `UNVERIFIED: <who claimed it>` — the one non-command form the
    /// schema accepts.
    Unverified(String),
    /// A command-plus-output block: at least one non-whitespace line,
    /// then a newline, then at least one more non-whitespace line.
    Evidence(String),
}

fn unverified_re() -> Regex {
    // Mirrors escalation.schema.json's `^UNVERIFIED: \S[\s\S]*$` half.
    Regex::new(r"^UNVERIFIED: \S[\s\S]*$").expect("static regex is valid")
}

fn evidence_re() -> Regex {
    // Mirrors escalation.schema.json's `[\s\S]*\S[\s\S]*\n[\s\S]*\S[\s\S]*`
    // half verbatim: non-whitespace, a newline, more non-whitespace.
    Regex::new(r"^[\s\S]*\S[\s\S]*\n[\s\S]*\S[\s\S]*$").expect("static regex is valid")
}

impl VerifiedBy {
    /// Validate `raw` against the schema's two accepted shapes, refusing
    /// prose **before any line is written** — the acceptance criterion this
    /// type exists to satisfy. Neither shape is inferred or repaired; a
    /// value that matches neither is an [`EscalationError::ProseVerifiedBy`],
    /// full stop.
    pub fn new(raw: impl Into<String>) -> Result<Self, EscalationError> {
        let raw = raw.into();
        if raw.is_empty() {
            return Err(EscalationError::EmptyField("verified_by"));
        }
        if unverified_re().is_match(&raw) {
            return Ok(Self::Unverified(raw));
        }
        if evidence_re().is_match(&raw) {
            return Ok(Self::Evidence(raw));
        }
        Err(EscalationError::ProseVerifiedBy(raw))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            VerifiedBy::Unverified(raw) | VerifiedBy::Evidence(raw) => raw.as_str(),
        }
    }
}

/// Why an [`EscalationRecord`] failed to compose. Every variant means the
/// record was refused before a line was ever written — no invalid
/// escalation is representable long enough to be serialized.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EscalationError {
    /// A required string field was empty.
    EmptyField(&'static str),
    /// `repo` or `lane` did not match the schema's slug pattern
    /// (`^[a-z0-9][a-z0-9-]*$`).
    BadSlug { field: &'static str, value: String },
    /// `ts_utc` did not match the schema's full ISO-8601 pattern.
    BadTimestamp(String),
    /// `verified_at_sha` did not match the schema's short-SHA pattern
    /// (`^[0-9a-f]{7,40}$`).
    BadSha(String),
    /// `summary` exceeded the schema's 1024-char cap (mirrors
    /// `crate::operator::limits::WHATSAPP_MAX_BODY_CHARS`).
    SummaryTooLong { chars: usize, max: usize },
    /// `verified_by` held prose — neither a command-plus-output block nor
    /// `UNVERIFIED: <who>`. This is the failure class Task 1's docs name
    /// explicitly: "at least one live record once carried all eleven
    /// fields and still failed on it."
    ProseVerifiedBy(String),
    /// An `EscalationOption`'s `label` exceeded [`OPTION_LABEL_MAX_CHARS`].
    OptionLabelTooLong {
        key: String,
        chars: usize,
        max: usize,
    },
    /// A caller tried to build [`EscalationOptions`] from a `Vec` whose
    /// length was neither 2 nor 3.
    WrongOptionCount { count: usize },
}

impl fmt::Display for EscalationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EscalationError::EmptyField(field) => {
                write!(f, "escalation field `{field}` must not be empty")
            }
            EscalationError::BadSlug { field, value } => write!(
                f,
                "escalation field `{field}` = {value:?} does not match the slug pattern ^[a-z0-9][a-z0-9-]*$"
            ),
            EscalationError::BadTimestamp(value) => write!(
                f,
                "escalation `ts_utc` = {value:?} is not a full ISO-8601 timestamp with Z or numeric offset"
            ),
            EscalationError::BadSha(value) => write!(
                f,
                "escalation `verified_at_sha` = {value:?} does not match ^[0-9a-f]{{7,40}}$"
            ),
            EscalationError::SummaryTooLong { chars, max } => write!(
                f,
                "escalation `summary` is {chars} characters, exceeds the {max}-char limit"
            ),
            EscalationError::ProseVerifiedBy(value) => write!(
                f,
                "escalation `verified_by` = {value:?} is prose, not a command-plus-output block or UNVERIFIED: <who> — refused at composition time"
            ),
            EscalationError::OptionLabelTooLong { key, chars, max } => write!(
                f,
                "escalation option '{key}' label is {chars} characters, exceeds the {max}-char limit"
            ),
            EscalationError::WrongOptionCount { count } => write!(
                f,
                "escalation notification channel needs 2 or 3 options, got {count}"
            ),
        }
    }
}

impl std::error::Error for EscalationError {}

fn slug_re() -> Regex {
    Regex::new(r"^[a-z0-9][a-z0-9-]*$").expect("static regex is valid")
}

fn timestamp_re() -> Regex {
    Regex::new(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d+)?(Z|[+-]\d{2}:\d{2})$")
        .expect("static regex is valid")
}

fn sha_re() -> Regex {
    Regex::new(r"^[0-9a-f]{7,40}$").expect("static regex is valid")
}

/// Maximum `summary` length — mirrors
/// `crate::operator::limits::WHATSAPP_MAX_BODY_CHARS` and
/// `escalation.schema.json`'s `summary.maxLength`.
pub const SUMMARY_MAX_CHARS: usize = 1024;

/// A composed, always-schema-valid escalation record — one line of
/// `planning/roadmaps/<roadmap>/escalations.jsonl`. The only way to obtain
/// one is [`EscalationRecord::new`] succeeding, so a value in hand is
/// already guaranteed to carry all eleven required fields in a shape
/// `check_escalations.py` accepts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EscalationRecord {
    ts_utc: String,
    repo: String,
    lane: String,
    kind: EscalationKind,
    severity: EscalationSeverity,
    channel: EscalationChannel,
    block: Option<String>,
    gate_id: String,
    summary: String,
    verified_by: VerifiedBy,
    durable_home: Value,
    /// The SUBJECT repo's short SHA — the repo the `verified_by` command
    /// actually ran in, never the brain root's (schema field docs; see
    /// this module's docs and `EN.15.G` Task 1's own description). This
    /// type cannot enforce *which* repo a caller measured the SHA in — that
    /// is a fact about how the caller obtained the string, not something
    /// the string's shape alone can prove — so the responsibility is
    /// named here for whoever calls [`EscalationRecord::new`].
    verified_at_sha: String,
    clears_when: Option<String>,
    host: Option<String>,
}

/// Builder-style construction args for [`EscalationRecord::new`], grouped
/// so the constructor does not take a dozen positional strings.
pub struct NewEscalation {
    pub ts_utc: String,
    pub repo: String,
    pub lane: String,
    pub kind: EscalationKind,
    pub severity: EscalationSeverity,
    pub channel: EscalationChannel,
    pub block: Option<String>,
    pub gate_id: String,
    pub summary: String,
    pub verified_by: String,
    pub durable_home: Value,
    pub verified_at_sha: String,
    pub clears_when: Option<String>,
    pub host: Option<String>,
}

impl EscalationRecord {
    /// Compose and validate an escalation record. Every one of the eleven
    /// required fields is checked against its schema shape here; any
    /// failure is a typed [`EscalationError`] and no [`EscalationRecord`]
    /// is produced — in particular, a prose `verified_by` never gets far
    /// enough to be written to disk.
    pub fn new(args: NewEscalation) -> Result<Self, EscalationError> {
        if args.ts_utc.is_empty() {
            return Err(EscalationError::EmptyField("ts_utc"));
        }
        if !timestamp_re().is_match(&args.ts_utc) {
            return Err(EscalationError::BadTimestamp(args.ts_utc));
        }
        if !slug_re().is_match(&args.repo) {
            return Err(EscalationError::BadSlug {
                field: "repo",
                value: args.repo,
            });
        }
        if !slug_re().is_match(&args.lane) {
            return Err(EscalationError::BadSlug {
                field: "lane",
                value: args.lane,
            });
        }
        if args.gate_id.is_empty() {
            return Err(EscalationError::EmptyField("gate_id"));
        }
        if args.summary.is_empty() {
            return Err(EscalationError::EmptyField("summary"));
        }
        if args.summary.chars().count() > SUMMARY_MAX_CHARS {
            return Err(EscalationError::SummaryTooLong {
                chars: args.summary.chars().count(),
                max: SUMMARY_MAX_CHARS,
            });
        }
        if !sha_re().is_match(&args.verified_at_sha) {
            return Err(EscalationError::BadSha(args.verified_at_sha));
        }
        let verified_by = VerifiedBy::new(args.verified_by)?;

        Ok(Self {
            ts_utc: args.ts_utc,
            repo: args.repo,
            lane: args.lane,
            kind: args.kind,
            severity: args.severity,
            channel: args.channel,
            block: args.block,
            gate_id: args.gate_id,
            summary: args.summary,
            verified_by,
            durable_home: args.durable_home,
            verified_at_sha: args.verified_at_sha,
            clears_when: args.clears_when,
            host: args.host,
        })
    }

    #[must_use]
    pub fn kind(&self) -> EscalationKind {
        self.kind
    }

    #[must_use]
    pub fn channel(&self) -> &EscalationChannel {
        &self.channel
    }

    #[must_use]
    pub fn repo(&self) -> &str {
        &self.repo
    }

    /// Render to the exact JSON object shape `check_escalations.py`
    /// validates: the eleven required keys, plus `block` / `clears_when` /
    /// `options` / `host` only when present (an absent optional field is
    /// omitted, never written as `null`, except `block` which the schema
    /// explicitly allows as `null`).
    #[must_use]
    pub fn to_json(&self) -> Value {
        let mut obj = Map::new();
        obj.insert("ts_utc".to_string(), json!(self.ts_utc));
        obj.insert("repo".to_string(), json!(self.repo));
        obj.insert("lane".to_string(), json!(self.lane));
        obj.insert("kind".to_string(), json!(self.kind.as_wire_str()));
        obj.insert("severity".to_string(), json!(self.severity.as_wire_str()));
        obj.insert("channel".to_string(), json!(self.channel.wire_value()));
        obj.insert("gate_id".to_string(), json!(self.gate_id));
        obj.insert("summary".to_string(), json!(self.summary));
        obj.insert("verified_by".to_string(), json!(self.verified_by.as_str()));
        obj.insert("durable_home".to_string(), self.durable_home.clone());
        obj.insert("verified_at_sha".to_string(), json!(self.verified_at_sha));
        if let Some(block) = &self.block {
            obj.insert("block".to_string(), json!(block));
        }
        if let Some(clears_when) = &self.clears_when {
            obj.insert("clears_when".to_string(), json!(clears_when));
        }
        if let Some(host) = &self.host {
            obj.insert("host".to_string(), json!(host));
        }
        if let Some(options) = self.channel.options() {
            obj.insert("options".to_string(), options.to_json());
        }
        Value::Object(obj)
    }

    /// Render as one `.jsonl` line — compact JSON followed by a trailing
    /// newline, ready to append to an escalations.jsonl file. Composition
    /// only; writing (and the append-only discipline — two writers share
    /// this file by design) is `integrate.rs`'s job (`EN.15.G` Task 2).
    #[must_use]
    pub fn to_jsonl_line(&self) -> String {
        format!(
            "{}\n",
            serde_json::to_string(&self.to_json()).expect("EscalationRecord always serializes")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_args() -> NewEscalation {
        NewEscalation {
            ts_utc: "2026-09-08T18:03:11Z".to_string(),
            repo: "engine-rs".to_string(),
            lane: "engine-rs-92".to_string(),
            kind: EscalationKind::Bail,
            severity: EscalationSeverity::Blocking,
            channel: EscalationChannel::session("dev-to-sweep-review").unwrap(),
            block: Some("EN.15.G".to_string()),
            gate_id: "coordination-layer-port/engine-rs/EN.15.G".to_string(),
            summary: "Task 3 bailed: no upstream symbol.".to_string(),
            verified_by: "cargo nextest run -p engine-core escalate::tests\nok".to_string(),
            durable_home: json!({"channel": "run-record", "ref": "engine-rs/planning/orchestration-run/coordination-layer-port/notes.md#en-15-g"}),
            verified_at_sha: "abc1234".to_string(),
            clears_when: None,
            host: None,
        }
    }

    #[test]
    fn composes_all_eleven_required_fields() {
        let record = EscalationRecord::new(valid_args()).expect("valid record");
        let json = record.to_json();
        let obj = json.as_object().expect("object");
        for field in [
            "ts_utc",
            "repo",
            "lane",
            "kind",
            "severity",
            "channel",
            "gate_id",
            "summary",
            "verified_by",
            "durable_home",
            "verified_at_sha",
        ] {
            assert!(obj.contains_key(field), "missing required field {field}");
        }
    }

    #[test]
    fn prose_verified_by_is_refused_at_composition_time() {
        let mut args = valid_args();
        args.verified_by = "measured".to_string();
        let err = EscalationRecord::new(args).expect_err("prose must be refused");
        assert!(matches!(err, EscalationError::ProseVerifiedBy(_)));
    }

    #[test]
    fn confirmed_alone_is_also_refused() {
        let mut args = valid_args();
        args.verified_by = "confirmed".to_string();
        assert!(EscalationRecord::new(args).is_err());
    }

    #[test]
    fn unverified_form_is_accepted() {
        let mut args = valid_args();
        args.verified_by = "UNVERIFIED: engine-rs-92".to_string();
        let record = EscalationRecord::new(args).expect("UNVERIFIED form is valid");
        assert_eq!(
            record.to_json()["verified_by"],
            json!("UNVERIFIED: engine-rs-92")
        );
    }

    #[test]
    fn empty_verified_by_is_refused() {
        let mut args = valid_args();
        args.verified_by = String::new();
        assert!(EscalationRecord::new(args).is_err());
    }

    #[test]
    fn four_options_cannot_be_constructed() {
        let options = vec![
            EscalationOption::new("a", "A").unwrap(),
            EscalationOption::new("b", "B").unwrap(),
            EscalationOption::new("c", "C").unwrap(),
            EscalationOption::new("d", "D").unwrap(),
        ];
        let err = EscalationOptions::new(options).expect_err("four options must be refused");
        assert_eq!(err, EscalationError::WrongOptionCount { count: 4 });
    }

    #[test]
    fn one_option_cannot_be_constructed() {
        let options = vec![EscalationOption::new("a", "A").unwrap()];
        assert!(EscalationOptions::new(options).is_err());
    }

    #[test]
    fn zero_options_cannot_be_constructed() {
        assert!(EscalationOptions::new(Vec::new()).is_err());
    }

    #[test]
    fn two_and_three_options_are_the_only_constructible_shapes() {
        let two = EscalationOptions::new(vec![
            EscalationOption::new("a", "A").unwrap(),
            EscalationOption::new("b", "B").unwrap(),
        ])
        .expect("two options is valid");
        assert_eq!(two.as_slice().len(), 2);

        let three = EscalationOptions::new(vec![
            EscalationOption::new("a", "A").unwrap(),
            EscalationOption::new("b", "B").unwrap(),
            EscalationOption::new("c", "C").unwrap(),
        ])
        .expect("three options is valid");
        assert_eq!(three.as_slice().len(), 3);
    }

    #[test]
    fn option_label_over_twenty_chars_is_refused() {
        let err = EscalationOption::new("a", "this label is definitely too long")
            .expect_err("label over 20 chars must be refused");
        assert!(matches!(err, EscalationError::OptionLabelTooLong { .. }));
    }

    #[test]
    fn notification_channel_requires_options_and_serializes_them() {
        let options = vec![
            EscalationOption::new("resume", "Resume").unwrap(),
            EscalationOption::new("abandon", "Abandon").unwrap(),
        ];
        let mut args = valid_args();
        args.channel = EscalationChannel::notification(options).unwrap();
        let record = EscalationRecord::new(args).expect("valid notification record");
        let json = record.to_json();
        assert_eq!(json["channel"], json!("notification"));
        assert_eq!(json["options"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn session_channel_never_carries_options() {
        let record = EscalationRecord::new(valid_args()).expect("valid session record");
        let json = record.to_json();
        assert_eq!(json["channel"], json!("session:dev-to-sweep-review"));
        assert!(json.get("options").is_none());
    }

    #[test]
    fn empty_session_slug_is_refused() {
        assert!(EscalationChannel::session("").is_err());
    }

    #[test]
    fn bad_gate_id_repo_slug_is_refused() {
        let mut args = valid_args();
        args.repo = "Engine_RS".to_string();
        let err =
            EscalationRecord::new(args).expect_err("uppercase/underscore repo must be refused");
        assert!(matches!(
            err,
            EscalationError::BadSlug { field: "repo", .. }
        ));
    }

    #[test]
    fn bad_timestamp_is_refused() {
        let mut args = valid_args();
        args.ts_utc = "2026-09-08".to_string();
        let err = EscalationRecord::new(args).expect_err("date-only timestamp must be refused");
        assert!(matches!(err, EscalationError::BadTimestamp(_)));
    }

    #[test]
    fn bad_sha_is_refused() {
        let mut args = valid_args();
        args.verified_at_sha = "not-a-sha".to_string();
        let err = EscalationRecord::new(args).expect_err("non-hex sha must be refused");
        assert!(matches!(err, EscalationError::BadSha(_)));
    }

    #[test]
    fn summary_over_limit_is_refused() {
        let mut args = valid_args();
        args.summary = "x".repeat(SUMMARY_MAX_CHARS + 1);
        let err = EscalationRecord::new(args).expect_err("oversized summary must be refused");
        assert!(matches!(err, EscalationError::SummaryTooLong { .. }));
    }

    #[test]
    fn jsonl_line_is_valid_json_with_trailing_newline() {
        let record = EscalationRecord::new(valid_args()).expect("valid record");
        let line = record.to_jsonl_line();
        assert!(line.ends_with('\n'));
        let parsed: Value = serde_json::from_str(line.trim_end()).expect("valid json");
        assert_eq!(parsed["kind"], json!("bail"));
    }

    #[test]
    fn block_field_is_included_when_present() {
        let record = EscalationRecord::new(valid_args()).expect("valid record");
        assert_eq!(record.to_json()["block"], json!("EN.15.G"));
    }

    #[test]
    fn block_field_is_omitted_when_absent() {
        let mut args = valid_args();
        args.block = None;
        let record = EscalationRecord::new(args).expect("valid record");
        assert!(record.to_json().get("block").is_none());
    }
}
