//! `doc_materializer` — the injectable seam `MaterializeDocNode` (`EN.7.A`
//! task 4) calls to write a `BrainDocModel`-shaped artifact into the Brain
//! corpus as a source `.md` document, via `mev`/`okf-core` in-process (D53's
//! fourth boundary-test channel: engine executes, mev writes the document,
//! Synapse still owns the derived index).
//!
//! Mirrors `crate::nodes::http_post`'s shape: a trait so production code
//! reaches for a real `mev`-backed writer ([`doc_materializer_live`]) while
//! tests inject a [`StubDocMaterializer`] that records the last call it was
//! handed — no filesystem writes in the gated `cargo test` suite unless a
//! test deliberately drives the live impl against its own `tempfile::tempdir()`.
//!
//! No `mev`/`okf-core` type appears in this module's public signature — only
//! the engine-owned [`MaterializedFile`]/[`MaterializeDiagnostic`]/
//! [`MaterializeOutcome`] triad, so callers (and `StubDocMaterializer`) never
//! need to name a mev type.
//!
//! **Model dispatch (deliberately duplicated, do not "fix"):** the live
//! impl's `model` -> planner dispatch below is the same ~20-line dispatch as
//! `mev::doc_materialize` (`"opportunity"` -> `mev::doc::opportunity::plan_ingest`
//! with kind auto-detection, `"learning-artifact"` -> `mev::doc::plan_document`
//! over an `okf_core::LearningArtifact::from_payload`-built model, `"proposal"`
//! -> `mev::doc::plan_document` over an `okf_core::Proposal::from_automation_roadmap`
//! model reading `company_name`/`roadmap` off `input`). Planning first — rather
//! than calling `mev::doc_materialize` directly, which folds planning and
//! `apply_plan` into one opaque `Report` — is deliberate: this block's eval
//! slice is "the node yields the correct `EmitPlan` / on-disk file", and only
//! the plan exposes the target paths before they are applied. A future reader
//! tempted to collapse this back to `mev::doc_materialize` would lose that
//! visibility.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::Value;

/// One file a materialize call planned to write (or did write), plus the
/// human note the planner attached to it (mirrors `mev::brain::emit::EmitAction`'s
/// `path`/`note` fields, without exposing the mev type itself).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaterializedFile {
    pub path: PathBuf,
    pub note: String,
}

/// One diagnostic a materialize call raised, mapped from `mev::Diagnostic`.
/// `code` carries mev's `Diagnostic.locator` field (the code lives there,
/// e.g. `I_EMIT_WROTE`, `W_EMIT_DRY_RUN`, `E_EMIT_WRITE_FAILED`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaterializeDiagnostic {
    pub severity: String,
    pub file: PathBuf,
    pub code: String,
    pub message: String,
}

impl MaterializeDiagnostic {
    /// Whether this diagnostic is error-severity (`"error"`).
    #[must_use]
    pub fn is_error(&self) -> bool {
        self.severity == "error"
    }
}

/// The outcome of one `DocMaterializer::materialize` call: whether anything
/// was actually written, the files planned (populated even in dry-run mode,
/// before `apply_plan` runs), and every diagnostic raised while planning or
/// applying.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MaterializeOutcome {
    pub wrote: bool,
    pub planned: Vec<MaterializedFile>,
    pub diagnostics: Vec<MaterializeDiagnostic>,
}

impl MaterializeOutcome {
    /// Every error-severity diagnostic.
    #[must_use]
    pub fn errors(&self) -> Vec<&MaterializeDiagnostic> {
        self.diagnostics.iter().filter(|d| d.is_error()).collect()
    }

    /// Every warning-severity diagnostic (anything not error-severity).
    #[must_use]
    pub fn warnings(&self) -> Vec<&MaterializeDiagnostic> {
        self.diagnostics.iter().filter(|d| !d.is_error()).collect()
    }
}

/// The three `model` values `MevDocMaterializer` dispatches on. Any other
/// value is an `Err` naming this list.
const VALID_MODELS: [&str; 3] = ["opportunity", "learning-artifact", "proposal"];

/// The injectable doc-materialize seam: plan + apply a `BrainDocModel`-shaped
/// write for `model`/`input` under `root`, honouring `write` (`false` is a
/// dry run — nothing touches disk, but `planned` still names the target
/// paths). Async trait so both the live `mev`-backed implementation and test
/// stubs can be swapped in behind a single `Arc<dyn DocMaterializer>`.
#[async_trait]
pub trait DocMaterializer: Send + Sync {
    async fn materialize(
        &self,
        root: &Path,
        model: &str,
        input: &Value,
        write: bool,
    ) -> Result<MaterializeOutcome, String>;
}

/// The live `DocMaterializer`: builds the `EmitPlan` via mev's planners using
/// the same model dispatch as `mev::doc_materialize` (see module doc), then
/// calls `mev::brain::emit::apply_plan`. mev's planners and `apply_plan` are
/// synchronous filesystem work, so the live impl runs them inside
/// `tokio::task::spawn_blocking` and awaits the join handle (the pattern
/// `EN.6.A`'s `channel_transport` used for `Workflow::run`'s non-`Send`
/// progress callback).
#[derive(Debug, Default, Clone, Copy)]
pub struct MevDocMaterializer;

/// Plan a `mev::brain::emit::EmitPlan` for `model`/`input` under `root`,
/// mirroring `mev::doc_materialize`'s dispatch. Returns `Err` naming the
/// three valid models for anything else.
fn plan_for_model(
    root: &Path,
    model: &str,
    input: &Value,
) -> Result<mev::brain::emit::EmitPlan, String> {
    use okf_core::{LearningArtifact, Proposal};

    let plan = match model {
        "opportunity" => mev::doc::opportunity::plan_ingest(input, None, root),
        "learning-artifact" => {
            let artifact = LearningArtifact::from_payload(input);
            mev::doc::plan_document(&artifact, root)
        }
        "proposal" => {
            let company_name = input
                .get("company_name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let roadmap = input.get("roadmap").cloned().unwrap_or(Value::Null);
            let proposal = Proposal::from_automation_roadmap(company_name, &roadmap);
            mev::doc::plan_document(&proposal, root)
        }
        other => {
            return Err(format!(
                "unknown model '{other}' (expected one of: {})",
                VALID_MODELS.join(" | ")
            ));
        }
    };

    Ok(plan)
}

/// Map one `mev::Diagnostic` into the engine-owned [`MaterializeDiagnostic`].
fn map_diagnostic(diag: &mev::Diagnostic) -> MaterializeDiagnostic {
    MaterializeDiagnostic {
        severity: diag.severity.to_string(),
        file: diag.file.clone(),
        code: diag.locator.clone(),
        message: diag.message.clone(),
    }
}

#[async_trait]
impl DocMaterializer for MevDocMaterializer {
    async fn materialize(
        &self,
        root: &Path,
        model: &str,
        input: &Value,
        write: bool,
    ) -> Result<MaterializeOutcome, String> {
        let root = root.to_path_buf();
        let model = model.to_string();
        let input = input.clone();

        tokio::task::spawn_blocking(move || {
            let plan = plan_for_model(&root, &model, &input)?;

            let planned: Vec<MaterializedFile> = plan
                .actions
                .iter()
                .map(|action| MaterializedFile {
                    path: action.path.clone(),
                    note: action.note.clone(),
                })
                .collect();

            let plan_diagnostics: Vec<MaterializeDiagnostic> =
                plan.diagnostics.iter().map(map_diagnostic).collect();

            let apply_diagnostics = mev::brain::emit::apply_plan(&plan, write);
            let mut diagnostics = plan_diagnostics;
            diagnostics.extend(apply_diagnostics.iter().map(map_diagnostic));

            let has_error = diagnostics.iter().any(MaterializeDiagnostic::is_error);
            let wrote = write && !planned.is_empty() && !has_error;

            Ok(MaterializeOutcome {
                wrote,
                planned,
                diagnostics,
            })
        })
        .await
        .map_err(|err| format!("doc materialize task panicked: {err}"))?
    }
}

/// Convenience constructor: an `Arc<dyn DocMaterializer>` wrapping
/// [`MevDocMaterializer`]. Production callers (`MaterializeDocNode::new`)
/// reach for this; tests build a [`StubDocMaterializer`] instead so the
/// gated suite never writes to the real Brain corpus unless a test
/// deliberately drives the live impl against its own tempdir.
#[must_use]
pub fn doc_materializer_live() -> Arc<dyn DocMaterializer> {
    Arc::new(MevDocMaterializer)
}

/// The `(root, model, input, write)` tuple a `StubDocMaterializer` call was
/// handed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedMaterializeCall {
    pub root: PathBuf,
    pub model: String,
    pub input: Value,
    pub write: bool,
}

/// Test-stub `DocMaterializer`: records the last `(root, model, input,
/// write)` call it was handed and returns a configurable
/// `Ok(MaterializeOutcome)` / `Err(String)` — same `Arc<Mutex<..>>`
/// interior-mutability shape as `StubHttpPost::succeeding`/`failing`.
#[derive(Clone)]
pub struct StubDocMaterializer {
    last_call: Arc<Mutex<Option<RecordedMaterializeCall>>>,
    result: Arc<Mutex<Result<MaterializeOutcome, String>>>,
}

impl StubDocMaterializer {
    /// A stub that always succeeds with the given outcome.
    #[must_use]
    pub fn succeeding(outcome: MaterializeOutcome) -> Self {
        Self {
            last_call: Arc::new(Mutex::new(None)),
            result: Arc::new(Mutex::new(Ok(outcome))),
        }
    }

    /// A stub that always fails with the given error message.
    #[must_use]
    pub fn failing(error: impl Into<String>) -> Self {
        Self {
            last_call: Arc::new(Mutex::new(None)),
            result: Arc::new(Mutex::new(Err(error.into()))),
        }
    }

    /// The `(root, model, input, write)` tuple passed to the most recent
    /// `materialize` call, if any.
    #[must_use]
    pub fn last_call(&self) -> Option<RecordedMaterializeCall> {
        self.last_call.lock().unwrap().clone()
    }
}

#[async_trait]
impl DocMaterializer for StubDocMaterializer {
    async fn materialize(
        &self,
        root: &Path,
        model: &str,
        input: &Value,
        write: bool,
    ) -> Result<MaterializeOutcome, String> {
        *self.last_call.lock().unwrap() = Some(RecordedMaterializeCall {
            root: root.to_path_buf(),
            model: model.to_string(),
            input: input.clone(),
            write,
        });
        self.result.lock().unwrap().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn stub_records_the_call_it_is_handed_and_returns_its_configured_result() {
        let stub = StubDocMaterializer::succeeding(MaterializeOutcome {
            wrote: true,
            planned: vec![MaterializedFile {
                path: PathBuf::from("/tmp/brain/opportunities/acme.md"),
                note: "created".to_string(),
            }],
            diagnostics: vec![],
        });

        let outcome = stub
            .materialize(
                Path::new("/tmp/brain"),
                "opportunity",
                &json!({"company_name": "Acme"}),
                true,
            )
            .await
            .expect("succeeding stub should return Ok");

        assert!(outcome.wrote);
        assert_eq!(outcome.planned.len(), 1);

        let call = stub.last_call().expect("call should be recorded");
        assert_eq!(call.root, PathBuf::from("/tmp/brain"));
        assert_eq!(call.model, "opportunity");
        assert_eq!(call.input, json!({"company_name": "Acme"}));
        assert!(call.write);
    }

    #[tokio::test]
    async fn stub_returns_a_configurable_failure() {
        let stub = StubDocMaterializer::failing("boom");

        let result = stub
            .materialize(Path::new("/tmp/brain"), "opportunity", &json!({}), true)
            .await;

        assert_eq!(result, Err("boom".to_string()));
        assert!(
            stub.last_call().is_some(),
            "a failing call is still recorded as attempted"
        );
    }

    #[test]
    fn live_impl_constructs_without_panicking() {
        let _live: Arc<dyn DocMaterializer> = doc_materializer_live();
    }

    /// A minimal `CompanyBrief`-shaped payload — enough for
    /// `Opportunity::from_company_brief`'s `detect_kind` auto-detection
    /// (`company_name` present) to resolve without depending on the sibling
    /// `mev` repo's fixture file (that copy is `EN.7.A` task 5's hermetic
    /// integration test, not this module's unit tests).
    fn company_brief_payload() -> Value {
        json!({
            "company_name": "Acme Corp",
            "summary": "Widget manufacturer expanding into SaaS.",
            "recent_developments": ["Raised Series B"],
            "pain_points": [],
            "outreach_hooks": [],
            "sources": [],
        })
    }

    #[tokio::test]
    async fn live_impl_plans_and_writes_an_opportunity_into_a_tempdir_root() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("business/docs/opportunities"))
            .expect("create opportunities dir");

        let input = company_brief_payload();

        let live = MevDocMaterializer;
        let outcome = live
            .materialize(dir.path(), "opportunity", &input, true)
            .await
            .expect("live materialize should succeed");

        assert!(outcome.wrote, "diagnostics: {:?}", outcome.diagnostics);
        assert_eq!(outcome.planned.len(), 1);
        let written_path = &outcome.planned[0].path;
        assert!(
            written_path.exists(),
            "expected {written_path:?} to have been written"
        );
    }

    #[tokio::test]
    async fn live_impl_dry_run_reports_planned_path_but_writes_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("business/docs/opportunities"))
            .expect("create opportunities dir");

        let input = company_brief_payload();

        let live = MevDocMaterializer;
        let outcome = live
            .materialize(dir.path(), "opportunity", &input, false)
            .await
            .expect("live materialize should succeed");

        assert!(!outcome.wrote);
        assert_eq!(outcome.planned.len(), 1);
        assert!(
            !outcome.planned[0].path.exists(),
            "dry run must not write to disk"
        );
    }

    #[tokio::test]
    async fn live_impl_unknown_model_returns_err_naming_valid_models() {
        let dir = tempfile::tempdir().expect("tempdir");

        let live = MevDocMaterializer;
        let err = live
            .materialize(dir.path(), "not-a-model", &json!({}), true)
            .await
            .expect_err("unknown model should error");

        assert!(err.contains("opportunity"));
        assert!(err.contains("learning-artifact"));
        assert!(err.contains("proposal"));
    }
}
