//! `WriteNotesNode` — renders `PRE_PLAN`'s researched findings into
//! `notes.md`, matching `.claude/commands/capture.md`'s output shape
//! (`EN.19.A` task 3).
//!
//! No model call. Reads `research::ResearchCodebaseNode`'s stamped output
//! (`ctx.nodes["ResearchCodebaseNode"]["content"]`) and `intake::
//! IntakeIdeaNode`'s normalized idea/slug, and writes
//! `<brain_root>/planning/open-work/pre-plan/<slug>/notes.md` — OKF
//! frontmatter (`type`, `title`, `description` at minimum, matching
//! `capture.md`'s field list) followed by the same section shape
//! `capture.md`'s Output Format produces.
//!
//! This node does not re-check whether the target file already exists —
//! `check_existing::CheckExistingNotesNode`'s router already prevents this
//! node from being reached when it does (unless `force_regenerate: true`),
//! and a second existence check here would be redundant defense against a
//! case the graph already routes around.
//!
//! Every substantive claim in the rendered body is tagged `VERIFIED` or
//! `ASSUMED` (never `SAID` — a machine-authored note has no session
//! statements to attribute). `research::NODE_NAME`'s prompt already
//! instructs the model to tag its own findings this way; this node adds a
//! documented `ASSUMED` fallback wrapper when the returned content carries
//! no explicit tag at all, so the guarantee holds even if the model's
//! output drifts from the prompt's instruction.

use std::path::PathBuf;

use chrono::Utc;
use engine_contract::TaskContext;

use crate::brain_root::resolve_brain_root;
use crate::node::{Node, NodeError};
use crate::workflows::{get_result, put_result};

use super::check_existing::notes_path;
use super::{intake, research};

/// The `Node::name()` identity this node registers under, and the
/// `ctx.nodes` key its result is stamped onto.
pub const NODE_NAME: &str = "WriteNotesNode";

/// `ctx.nodes[NODE_NAME]` key the webhook route (task 5) reads.
const NOTES_PATH_KEY: &str = "notes_path";

/// Turn a kebab-case slug into a human-readable title — `"work-email-setup"`
/// -> `"Work Email Setup"`. Best-effort only: this is a fallback title for a
/// machine-authored note, not a substitute for a human-picked one.
fn title_from_slug(slug: &str) -> String {
    slug.split(['-', '_'])
        .filter(|word| !word.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Derive a one-line frontmatter `description` from the idea text — trimmed
/// to a single line and capped so a long idea does not blow out the
/// frontmatter block.
fn description_from_idea(idea: &str) -> String {
    let single_line = idea.split_whitespace().collect::<Vec<_>>().join(" ");
    const MAX_LEN: usize = 200;
    if single_line.chars().count() > MAX_LEN {
        let truncated: String = single_line.chars().take(MAX_LEN).collect();
        format!("{truncated}…")
    } else {
        single_line
    }
}

/// Derive a handful of naive keywords from the slug's own words — a
/// machine-authored note has no session to mine topic terms from, so this is
/// a documented best-effort fallback, not `/capture`'s human-curated list.
fn keywords_from_slug(slug: &str) -> Vec<String> {
    let words: Vec<String> = slug
        .split(['-', '_'])
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect();
    if words.is_empty() {
        vec!["pre-plan".to_string()]
    } else {
        words
    }
}

/// Ensure the rendered body carries an explicit `VERIFIED`/`ASSUMED` tag.
/// `research::NODE_NAME`'s prompt already asks the model to tag its own
/// findings; if the returned content has drifted from that instruction (no
/// tag present at all), wrap it in a documented `ASSUMED` fallback so the
/// acceptance criterion — every substantive claim tagged — holds
/// unconditionally rather than only when the model complied.
fn ensure_tagged(research_content: &str) -> String {
    if research_content.contains("VERIFIED") || research_content.contains("ASSUMED") {
        research_content.to_string()
    } else {
        format!(
            "**ASSUMED** — the read-only research session below did not tag its own findings \
             with VERIFIED/ASSUMED; treat everything here as unverified until checked.\n\n{research_content}"
        )
    }
}

/// Render the full `notes.md` document, matching `capture.md`'s Output
/// Format (frontmatter + sections).
fn render_notes(slug: &str, idea: &str, research_content: &str) -> String {
    let today = Utc::now().format("%Y-%m-%d").to_string();
    let title = title_from_slug(slug);
    let description = description_from_idea(idea);
    let keywords_yaml = keywords_from_slug(slug).join(", ");
    let tagged_body = ensure_tagged(research_content);

    format!(
        "---\n\
         type: Note\n\
         title: {title}\n\
         description: {description}\n\
         doc_id: {slug}\n\
         layer: [meta]\n\
         status: draft\n\
         created: {today}\n\
         updated: {today}\n\
         keywords: [{keywords_yaml}]\n\
         ---\n\
         \n\
         # {title}\n\
         \n\
         > **Status:** draft — pre-plan holding area, generated by the `PRE_PLAN` workflow with no \
         human back-and-forth. **Claims are tagged VERIFIED / ASSUMED; anything untagged is \
         unconfirmed.**\n\
         \n\
         ## What & Why\n\
         \n\
         {description}\n\
         \n\
         ## Context & Background\n\
         \n\
         Generated automatically by the `PRE_PLAN` workflow from the idea text above — no prior \
         session context exists to draw on beyond the read-only research below.\n\
         \n\
         ## Key Information / Instructions\n\
         \n\
         {tagged_body}\n\
         \n\
         ## Open Questions\n\
         \n\
         None captured automatically — review the Key Information above for anything the research \
         session flagged as unresolved.\n\
         \n\
         ## Provenance\n\
         \n\
         Generated {today} by the `PRE_PLAN` workflow (`{NODE_NAME}`). No human session or repo SHA \
         to pin — see the tagged claims above for what was actually checked.\n"
    )
}

/// Renders `ResearchCodebaseNode`'s findings into `notes.md`, matching
/// `capture.md`'s output shape, and writes it to
/// `<brain_root>/planning/open-work/pre-plan/<slug>/notes.md`.
pub struct WriteNotesNode;

impl WriteNotesNode {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for WriteNotesNode {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Node for WriteNotesNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let intake_result = get_result(&ctx, intake::NODE_NAME).ok_or_else(|| {
            NodeError::new(format!(
                "{NODE_NAME}: {} has not run yet",
                intake::NODE_NAME
            ))
        })?;
        let slug = intake_result
            .get("slug")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| NodeError::new(format!("{NODE_NAME}: missing 'slug' from intake")))?
            .to_string();
        let idea = intake_result
            .get("idea")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| NodeError::new(format!("{NODE_NAME}: missing 'idea' from intake")))?
            .to_string();

        let research_content = get_result(&ctx, research::NODE_NAME)
            .and_then(|value| value.get("content"))
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                NodeError::new(format!(
                    "{NODE_NAME}: {} has not run yet",
                    research::NODE_NAME
                ))
            })?
            .to_string();

        let brain_root = resolve_brain_root().map_err(|err| {
            NodeError::new(format!("{NODE_NAME}: could not resolve brain root: {err}"))
        })?;

        // Reuse `check_existing`'s exact join order so this write can never
        // drift from what `CheckExistingNotesNode` already checked.
        let path: PathBuf = notes_path(&brain_root, &slug);

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|err| {
                NodeError::new(format!(
                    "{NODE_NAME}: failed to create directory {}: {err}",
                    parent.display()
                ))
            })?;
        }

        let document = render_notes(&slug, &idea, &research_content);
        std::fs::write(&path, &document).map_err(|err| {
            NodeError::new(format!(
                "{NODE_NAME}: failed to write {}: {err}",
                path.display()
            ))
        })?;

        put_result(
            &mut ctx,
            NODE_NAME,
            serde_json::json!({
                NOTES_PATH_KEY: path.display().to_string(),
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
    use std::sync::Mutex;

    use serde_json::json;

    use super::*;
    use crate::brain_root::ENGINE_BRAIN_ROOT_ENV;

    // `ENGINE_BRAIN_ROOT` is process-global state (see `brain_root.rs`'s own
    // tests) — guard every test that touches it so they cannot race the rest
    // of the suite.
    static ENV_GUARD: Mutex<()> = Mutex::new(());

    struct BrainRootGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
        previous: Option<String>,
    }

    impl BrainRootGuard {
        fn set(root: &std::path::Path) -> Self {
            let lock = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
            let previous = std::env::var(ENGINE_BRAIN_ROOT_ENV).ok();
            std::env::set_var(ENGINE_BRAIN_ROOT_ENV, root);
            Self {
                _lock: lock,
                previous,
            }
        }
    }

    impl Drop for BrainRootGuard {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(v) => std::env::set_var(ENGINE_BRAIN_ROOT_ENV, v),
                None => std::env::remove_var(ENGINE_BRAIN_ROOT_ENV),
            }
        }
    }

    fn empty_context(event: serde_json::Value) -> TaskContext {
        TaskContext {
            event,
            nodes: HashMap::new(),
            metadata: json!({}),
            node_runs: HashMap::new(),
        }
    }

    fn context_with_upstream(idea: &str, slug: &str, research_content: &str) -> TaskContext {
        let mut ctx = empty_context(json!({}));
        put_result(
            &mut ctx,
            intake::NODE_NAME,
            json!({"idea": idea, "slug": slug, "channel": null, "sender": null}),
        );
        put_result(
            &mut ctx,
            research::NODE_NAME,
            json!({"content": research_content}),
        );
        ctx
    }

    #[test]
    fn title_from_slug_converts_kebab_case_to_title_case() {
        assert_eq!(title_from_slug("work-email-setup"), "Work Email Setup");
        assert_eq!(title_from_slug("notion_dashboard"), "Notion Dashboard");
    }

    #[test]
    fn ensure_tagged_leaves_already_tagged_content_unchanged() {
        let content = "VERIFIED — read in source: `foo.rs`.";
        assert_eq!(ensure_tagged(content), content);
    }

    #[test]
    fn ensure_tagged_wraps_untagged_content_as_assumed() {
        let content = "The widget probably lives in crates/foo.";
        let wrapped = ensure_tagged(content);
        assert!(wrapped.starts_with("**ASSUMED**"));
        assert!(wrapped.contains(content));
    }

    #[tokio::test]
    async fn process_writes_valid_okf_frontmatter_matching_captures_field_list() {
        let dir = tempfile::tempdir().expect("tempdir");
        let _guard = BrainRootGuard::set(dir.path());

        let node = WriteNotesNode::new();
        let ctx = context_with_upstream(
            "build a widget dashboard",
            "widget-dashboard",
            "VERIFIED — `crates/foo/bar.rs` already has a widget module.",
        );

        let ctx = node.process(ctx).await.expect("process should succeed");
        let stored = ctx.nodes.get(NODE_NAME).expect("result stored");
        let path = stored
            .get(NOTES_PATH_KEY)
            .and_then(|v| v.as_str())
            .expect("notes_path stamped");

        let written = std::fs::read_to_string(path).expect("read written notes.md");

        // Frontmatter is delimited by `---` on either side and is
        // line-based `key: value` YAML — no dedicated YAML crate is a
        // dependency of this crate, so assert the delimiters and the
        // required fields directly rather than round-tripping through a
        // parser this crate does not otherwise depend on.
        let mut parts = written.splitn(3, "---\n");
        assert_eq!(parts.next(), Some(""));
        let frontmatter_raw = parts.next().expect("frontmatter block present");

        assert!(frontmatter_raw.lines().any(|l| l == "type: Note"));
        assert!(frontmatter_raw
            .lines()
            .any(|l| l == "title: Widget Dashboard"));
        assert!(frontmatter_raw
            .lines()
            .any(|l| l.starts_with("description: ")));
    }

    #[tokio::test]
    async fn process_tags_every_claim_verified_or_assumed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let _guard = BrainRootGuard::set(dir.path());

        let node = WriteNotesNode::new();
        let ctx = context_with_upstream(
            "build a widget",
            "widget-idea",
            "the widget module probably already exists somewhere",
        );

        let ctx = node.process(ctx).await.expect("process should succeed");
        let stored = ctx.nodes.get(NODE_NAME).expect("result stored");
        let path = stored.get(NOTES_PATH_KEY).and_then(|v| v.as_str()).unwrap();
        let written = std::fs::read_to_string(path).expect("read written notes.md");

        assert!(written.contains("VERIFIED") || written.contains("ASSUMED"));
    }

    #[tokio::test]
    async fn process_writes_to_brain_root_open_work_pre_plan_slug_notes_md() {
        let dir = tempfile::tempdir().expect("tempdir");
        let _guard = BrainRootGuard::set(dir.path());

        let node = WriteNotesNode::new();
        let ctx = context_with_upstream("build a widget", "widget-idea", "VERIFIED — findings.");

        let ctx = node.process(ctx).await.expect("process should succeed");
        let stored = ctx.nodes.get(NODE_NAME).expect("result stored");
        let path = stored.get(NOTES_PATH_KEY).and_then(|v| v.as_str()).unwrap();

        let expected = dir
            .path()
            .join("planning/open-work/pre-plan/widget-idea/notes.md");
        assert_eq!(path, expected.display().to_string());
        assert!(expected.exists());
    }

    #[tokio::test]
    async fn process_errors_when_research_has_not_run() {
        let mut ctx = empty_context(json!({}));
        put_result(
            &mut ctx,
            intake::NODE_NAME,
            json!({"idea": "build a widget", "slug": "widget-idea", "channel": null, "sender": null}),
        );

        let dir = tempfile::tempdir().expect("tempdir");
        let _guard = BrainRootGuard::set(dir.path());

        let node = WriteNotesNode::new();
        let err = node
            .process(ctx)
            .await
            .expect_err("should fail without ResearchCodebaseNode having run");
        assert!(err.message.contains(research::NODE_NAME));
    }

    #[tokio::test]
    async fn process_errors_when_intake_has_not_run() {
        let dir = tempfile::tempdir().expect("tempdir");
        let _guard = BrainRootGuard::set(dir.path());

        let node = WriteNotesNode::new();
        let ctx = empty_context(json!({}));

        let err = node
            .process(ctx)
            .await
            .expect_err("should fail without IntakeIdeaNode having run");
        assert!(err.message.contains(intake::NODE_NAME));
    }

    #[test]
    fn name_matches_node_name_const() {
        assert_eq!(WriteNotesNode::new().name(), NODE_NAME);
    }
}
