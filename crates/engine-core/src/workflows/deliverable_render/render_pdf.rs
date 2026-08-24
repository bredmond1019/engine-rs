//! `RenderPdfNode` — the deterministic subprocess node that invokes `typst`
//! to render `<company-slug>-roadmap.md` -> `<company-slug>-roadmap.pdf`,
//! both under the event's `output_dir`, over the injectable
//! `crate::workflows::CommandRunner` seam.
//!
//! Mirrors `sdlc_flow::end_review::EndReviewNode`'s `with_runner` builder
//! pattern exactly: the default runner is [`default_command_runner`] (the
//! real subprocess, gated by the non-overridable
//! [`crate::policy::command_floor::evaluate_command`] org-floor denylist),
//! and tests substitute a stub via [`RenderPdfNode::with_runner`] so the
//! gated `cargo nextest` suite NEVER shells out to a real `typst` — which is
//! confirmed absent on this host as of 2026-08-24 (`command -v typst` ->
//! not found).
//!
//! **`typst` clears the command floor.** Checked against all five
//! `FLOOR_RULE_DEFS` in `crate::policy::command_floor` (recursive `rm`,
//! force push, destructive SQL, mkfs/fork-bomb, pipe-to-shell) — none match
//! a `typst compile <in> <out>` invocation, so [`default_command_runner`]
//! does not deny it.

use engine_contract::TaskContext;
use serde_json::json;

use crate::node::{Node, NodeError};
use crate::workflows::{default_command_runner, put_result, CommandRunner};

use super::schema::{deliverable_slug, DeliverableRenderEventSchema};

/// The `Node::name()` identity `RenderPdfNode` runs under, and the
/// `ctx.nodes` key its result is stamped onto.
pub const NODE_NAME: &str = "RenderPdfNode";

/// The `typst` subcommand this node invokes.
const TYPST_SUBCOMMAND: &str = "compile";

/// Deserialize the inbound `DELIVERABLE_RENDER` event from `ctx.event`.
fn parse_event(ctx: &TaskContext) -> Result<DeliverableRenderEventSchema, NodeError> {
    serde_json::from_value(ctx.event.clone())
        .map_err(|err| NodeError::new(format!("invalid DELIVERABLE_RENDER event: {err}")))
}

/// The `typst compile <markdown_path> <pdf_path>` argv this node invokes,
/// pinned as its own function so a test can assert the exact shape without
/// driving the whole node, and so the fixture-evidence test (task 7) can
/// print it verbatim as a hand-verification command.
#[must_use]
pub fn typst_argv(markdown_path: &str, pdf_path: &str) -> Vec<String> {
    vec![
        TYPST_SUBCOMMAND.to_string(),
        markdown_path.to_string(),
        pdf_path.to_string(),
    ]
}

/// The deterministic subprocess node that invokes `typst` to render
/// `<company-slug>-roadmap.md` -> `<company-slug>-roadmap.pdf`, both under
/// the event's `output_dir`, over the injectable [`CommandRunner`] seam.
///
/// A non-zero exit or a missing `typst` binary (an `Err` from the runner)
/// surfaces as a [`NodeError`] carrying the runner's stderr, never a silent
/// success.
pub struct RenderPdfNode {
    runner: CommandRunner,
}

impl RenderPdfNode {
    #[must_use]
    pub fn new() -> Self {
        Self {
            runner: default_command_runner(),
        }
    }

    /// Override the command runner used for the `typst` invocation — tests
    /// substitute a stub so the gated suite never shells out.
    #[must_use]
    pub fn with_runner(mut self, runner: CommandRunner) -> Self {
        self.runner = runner;
        self
    }
}

impl Default for RenderPdfNode {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Node for RenderPdfNode {
    async fn process(&self, ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let event = parse_event(&ctx)?;
        let slug = deliverable_slug(&event.roadmap);

        let markdown_path = event.output_dir.join(format!("{slug}-roadmap.md"));
        let pdf_path = event.output_dir.join(format!("{slug}-roadmap.pdf"));
        let markdown_path_str = markdown_path.display().to_string();
        let pdf_path_str = pdf_path.display().to_string();

        let argv = typst_argv(&markdown_path_str, &pdf_path_str);
        let arg_refs: Vec<&str> = argv.iter().map(String::as_str).collect();

        let output = (self.runner)("typst", &arg_refs, &event.output_dir).map_err(|err| {
            NodeError::new(format!(
                "{NODE_NAME}: failed to invoke typst on {markdown_path_str}: {err}"
            ))
        })?;

        if output.status != 0 {
            return Err(NodeError::new(format!(
                "{NODE_NAME}: typst exited with status {status} rendering {markdown_path_str}: \
                 {stderr}",
                status = output.status,
                stderr = output.stderr,
            )));
        }

        let mut ctx = ctx;
        put_result(
            &mut ctx,
            NODE_NAME,
            json!({
                "markdown_path": markdown_path_str,
                "pdf_path": pdf_path_str,
                "company_slug": slug,
            }),
        );

        Ok(ctx)
    }

    fn name(&self) -> &str {
        NODE_NAME
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::locale::Locale;
    use crate::workflows::proposal_generator::schema::{
        AutomationRoadmap, SituationAndOpportunity,
    };
    use crate::workflows::CommandOutput;

    static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_dir() -> PathBuf {
        let n = TMP_COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "engine-core-render-pdf-test-{}-{n}",
            std::process::id()
        ));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn roadmap_with_company(company_name: &str) -> AutomationRoadmap {
        AutomationRoadmap {
            situation: Some(SituationAndOpportunity {
                company_name: company_name.to_string(),
                business_type: "retail SMB".to_string(),
                team_size: 4,
                painful_workflow_summary: "manual tracking".to_string(),
                candidate_count: 2,
            }),
            authored_locale: Locale::EnUs,
            ..Default::default()
        }
    }

    fn base_ctx(event: DeliverableRenderEventSchema) -> TaskContext {
        TaskContext {
            event: serde_json::to_value(event).unwrap(),
            nodes: HashMap::new(),
            metadata: serde_json::json!({}),
            node_runs: HashMap::new(),
        }
    }

    /// A stub `CommandRunner` that records every call it receives and
    /// returns a fixed `CommandOutput`, never touching a real subprocess.
    fn stub_runner(
        output: CommandOutput,
    ) -> (
        CommandRunner,
        Arc<Mutex<Vec<(String, Vec<String>, PathBuf)>>>,
    ) {
        let calls: Arc<Mutex<Vec<(String, Vec<String>, PathBuf)>>> =
            Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&calls);
        let runner: CommandRunner = Arc::new(move |program, args, cwd| {
            recorded.lock().unwrap().push((
                program.to_string(),
                args.iter().map(|s| s.to_string()).collect(),
                cwd.to_path_buf(),
            ));
            Ok(output.clone())
        });
        (runner, calls)
    }

    fn success_output() -> CommandOutput {
        CommandOutput {
            status: 0,
            stdout: String::new(),
            stderr: String::new(),
        }
    }

    // --- argv shape -----------------------------------------------------

    #[test]
    fn typst_argv_pins_the_expected_shape() {
        let argv = typst_argv("/tmp/out/acme-roadmap.md", "/tmp/out/acme-roadmap.pdf");
        assert_eq!(
            argv,
            vec![
                "compile".to_string(),
                "/tmp/out/acme-roadmap.md".to_string(),
                "/tmp/out/acme-roadmap.pdf".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn process_invokes_the_runner_with_program_typst_and_the_expected_argv() {
        let output_dir = temp_dir();
        let (runner, calls) = stub_runner(success_output());
        let event = DeliverableRenderEventSchema {
            roadmap: roadmap_with_company("Acme"),
            locale: Locale::EnUs,
            output_dir: output_dir.clone(),
            policy: None,
            profile: None,
        };
        let ctx = base_ctx(event);

        let node = RenderPdfNode::new().with_runner(runner);
        node.process(ctx).await.expect("process should succeed");

        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        let (program, args, cwd) = &calls[0];
        assert_eq!(program, "typst");
        let expected_md = output_dir.join("acme-roadmap.md").display().to_string();
        let expected_pdf = output_dir.join("acme-roadmap.pdf").display().to_string();
        assert_eq!(
            args,
            &vec!["compile".to_string(), expected_md, expected_pdf]
        );
        assert_eq!(cwd, &output_dir);
    }

    #[tokio::test]
    async fn process_reports_both_paths_under_output_dir_in_ctx_nodes() {
        let output_dir = temp_dir();
        let (runner, _calls) = stub_runner(success_output());
        let event = DeliverableRenderEventSchema {
            roadmap: roadmap_with_company("Acme"),
            locale: Locale::EnUs,
            output_dir: output_dir.clone(),
            policy: None,
            profile: None,
        };
        let ctx = base_ctx(event);

        let node = RenderPdfNode::new().with_runner(runner);
        let ctx = node.process(ctx).await.expect("process should succeed");

        let result = &ctx.nodes[NODE_NAME];
        assert_eq!(
            result["markdown_path"],
            json!(output_dir.join("acme-roadmap.md").display().to_string())
        );
        assert_eq!(
            result["pdf_path"],
            json!(output_dir.join("acme-roadmap.pdf").display().to_string())
        );
        assert_eq!(result["company_slug"], json!("acme"));
    }

    #[tokio::test]
    async fn a_nonzero_exit_returns_a_node_error_containing_the_stub_stderr() {
        let output_dir = temp_dir();
        let failure = CommandOutput {
            status: 1,
            stdout: String::new(),
            stderr: "typst: error: file not found".to_string(),
        };
        let (runner, _calls) = stub_runner(failure);
        let event = DeliverableRenderEventSchema {
            roadmap: roadmap_with_company("Acme"),
            locale: Locale::EnUs,
            output_dir,
            policy: None,
            profile: None,
        };
        let ctx = base_ctx(event);

        let node = RenderPdfNode::new().with_runner(runner);
        let err = node.process(ctx).await.expect_err("should fail");
        assert!(err.message.contains("typst: error: file not found"));
        assert!(err.message.contains('1'));
    }

    #[tokio::test]
    async fn a_missing_typst_binary_surfaces_as_a_node_error_not_a_silent_success() {
        let output_dir = temp_dir();
        let runner: CommandRunner = Arc::new(|_program, _args, _cwd| {
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "No such file or directory (os error 2)",
            ))
        });
        let event = DeliverableRenderEventSchema {
            roadmap: roadmap_with_company("Acme"),
            locale: Locale::EnUs,
            output_dir,
            policy: None,
            profile: None,
        };
        let ctx = base_ctx(event);

        let node = RenderPdfNode::new().with_runner(runner);
        let err = node.process(ctx).await.expect_err("should fail");
        assert!(err.message.contains("failed to invoke typst"));
    }

    #[tokio::test]
    async fn process_errors_on_an_invalid_event() {
        let ctx = TaskContext {
            event: json!({ "not_a_valid_event": true }),
            nodes: HashMap::new(),
            metadata: json!({}),
            node_runs: HashMap::new(),
        };
        let node = RenderPdfNode::new();
        let err = node.process(ctx).await.expect_err("should fail");
        assert!(err.message.contains("invalid DELIVERABLE_RENDER event"));
    }
}
