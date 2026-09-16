//! `SecretGuardNode` — `PRE_PLAN`'s backend-agnostic persistence guard
//! (`EN.19.A` task 9, revised AC6).
//!
//! The original AC6 wording asked for the research session's own *read* of a
//! secret-shaped file to be impossible. That guarantee is backend-specific
//! (only `claude-code-rs`'s own `PreToolUse` hook mechanism — ticketed
//! separately as `CC.ticket.enforced-secret-path-denial-hook` — can enforce
//! it, and only for the `claude_cli` backend) and is not achievable
//! in-process across all three `AgentBackend`s (`claude_cli`/`pi`/`aider`)
//! without OS sandboxing. This node targets the artifact that actually
//! leaves this process instead: it scans
//! `research::ResearchCodebaseNode`'s output for a verbatim line lifted from
//! a secret-shaped file under the scan root, and — on a match — errors
//! *before* `write_notes::WriteNotesNode` ever runs (task 10 wires this node
//! between `research` and `write_notes` in the graph), so a leak never
//! reaches `notes.md` regardless of which backend produced the research
//! output.
//!
//! This is deliberately NOT a re-implementation of `claude-code-rs`'s own
//! tool-scoping (that already blocks `Write`/`Bash`/`Edit` per
//! `research.rs`'s read-only `Config`) — it is a second, independent net
//! over the one artifact (`notes.md`) that must never carry a leak, so a gap
//! in one layer (a backend that doesn't honour `disallowed_tools`, or the
//! model narrating file content it read via an allowed `Read`) doesn't
//! silently become a leaked secret in the corpus.
//!
//! `find_secret_files`/`text_contains_any_line_from` are plain, hermetically
//! testable helpers with no `Node`/`TaskContext` dependency — `SecretGuardNode`
//! is a thin `Node` wrapper composing the two.

use std::fs;
use std::path::{Path, PathBuf};

use engine_contract::TaskContext;
use regex::Regex;

use crate::brain_root::resolve_brain_root;
use crate::node::{Node, NodeError};
use crate::workflows::{get_result, put_result};

use super::research;
use super::PrePlanPolicy;

/// The `Node::name()` identity this node registers under, and the
/// `ctx.nodes` key its verdict is stamped onto.
pub const NODE_NAME: &str = "SecretGuardNode";

/// `ctx.nodes[NODE_NAME]` key stamped on a clean pass-through.
const CHECKED_KEY: &str = "checked";

/// A verbatim-line match must be at least this many characters to count —
/// mirrors `~/.claude/hooks/deny-secret-paths.py`'s own discipline of never
/// treating a short/blank line as evidence, which would false-positive on
/// nearly any two files that happen to share a one-word line.
const MIN_LINE_LEN: usize = 12;

/// Directory names pruned before descending — build/VCS/dependency trees
/// that are never where a secret-shaped file legitimately lives and that
/// would otherwise make every scan slow (and, for `.git`, could itself
/// contain literal secret-shaped blobs from history).
const PRUNED_DIR_NAMES: [&str; 5] = [".git", "target", "node_modules", "dist", ".venv"];

/// The built-in `secret_guard_patterns` default — byte-identical to
/// `~/.claude/hooks/deny-secret-paths.py`'s own `DENIED_PATTERNS`, the
/// global `PreToolUse` deny-list already validated this session, so this
/// node's notion of "secret-shaped" matches the fleet's existing one rather
/// than inventing a second, divergent list.
pub const DEFAULT_SECRET_GUARD_PATTERNS: &[&str] = &[
    ".env",
    ".env.*",
    "*.pem",
    "id_rsa",
    "id_rsa.*",
    "id_ed25519",
    "id_ed25519.*",
    "*credentials*.json",
    "*secrets*.json",
    "*secret*.yaml",
    "*secret*.yml",
    ".npmrc",
    ".pypirc",
    ".netrc",
    "*.p12",
    "*.pfx",
    "*.keychain",
    "*_rsa",
    "*.key",
];

/// Build [`PrePlanPolicy::default`]'s `secret_guard_patterns` value.
#[must_use]
pub fn default_patterns() -> Vec<String> {
    DEFAULT_SECRET_GUARD_PATTERNS
        .iter()
        .map(|p| (*p).to_string())
        .collect()
}

/// Compile one `fnmatch`-style glob pattern (`*` = any run of characters,
/// every other character literal) into a case-insensitive, whole-string
/// `Regex`. Mirrors Python's `fnmatch.fnmatch` semantics for the subset of
/// glob syntax `DEFAULT_SECRET_GUARD_PATTERNS` actually uses (no `?`/`[...]`
/// classes needed by any pattern above, so this stays deliberately narrow
/// rather than pulling in a full glob crate for one wildcard character).
fn pattern_to_regex(pattern: &str) -> Regex {
    let mut source = String::from("(?i)^");
    for ch in pattern.chars() {
        if ch == '*' {
            source.push_str(".*");
        } else if ch.is_alphanumeric() || ch == '_' || ch == '-' {
            source.push(ch);
        } else {
            source.push('\\');
            source.push(ch);
        }
    }
    source.push('$');
    Regex::new(&source).unwrap_or_else(|err| {
        panic!("pattern_to_regex built an invalid regex from '{pattern}': {err}")
    })
}

/// Recursively walk `root`, pruning [`PRUNED_DIR_NAMES`] before descending
/// into them, and return every file whose basename case-insensitively
/// `fnmatch`es one of `patterns`. Read-only: never creates, modifies, or
/// deletes anything on disk.
#[must_use]
pub fn find_secret_files(root: &Path, patterns: &[String]) -> Vec<PathBuf> {
    let regexes: Vec<Regex> = patterns.iter().map(|p| pattern_to_regex(p)).collect();
    let mut matches = Vec::new();
    let mut stack = vec![root.to_path_buf()];

    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            if file_type.is_dir() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if PRUNED_DIR_NAMES.contains(&name.as_ref()) {
                    continue;
                }
                stack.push(path);
            } else if file_type.is_file() {
                let basename = entry.file_name();
                let basename = basename.to_string_lossy();
                if regexes.iter().any(|re| re.is_match(&basename)) {
                    matches.push(path);
                }
            }
        }
    }

    matches.sort();
    matches
}

/// Return the first of `needle_files` that has at least one non-blank line
/// of length `>= min_line_len` (after trimming) appearing verbatim inside
/// `haystack`. Blank/short lines are skipped so a common one-word line
/// (`"true"`, `"---"`, an empty line) never registers as a match. A needle
/// file that cannot be read is skipped, not treated as an error — this is a
/// best-effort scan over whatever is actually on disk, mirroring
/// [`find_secret_files`]'s own fail-open-per-entry behavior.
#[must_use]
pub fn text_contains_any_line_from(
    haystack: &str,
    needle_files: &[PathBuf],
    min_line_len: usize,
) -> Option<PathBuf> {
    for file in needle_files {
        let Ok(content) = fs::read_to_string(file) else {
            continue;
        };
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.len() < min_line_len {
                continue;
            }
            if haystack.contains(trimmed) {
                return Some(file.clone());
            }
        }
    }
    None
}

/// Blocks `notes.md` persistence when `research::ResearchCodebaseNode`'s
/// output appears to carry a verbatim line lifted from a secret-shaped file
/// under the resolved scan root. No model call.
pub struct SecretGuardNode {
    policy: PrePlanPolicy,
}

impl SecretGuardNode {
    #[must_use]
    pub fn new(policy: PrePlanPolicy) -> Self {
        Self { policy }
    }
}

impl Default for SecretGuardNode {
    fn default() -> Self {
        Self::new(PrePlanPolicy::default())
    }
}

#[async_trait::async_trait]
impl Node for SecretGuardNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
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

        let scan_root = match &self.policy.secret_guard_scan_root {
            Some(root) => PathBuf::from(root),
            None => resolve_brain_root().map_err(|err| {
                NodeError::new(format!("{NODE_NAME}: could not resolve brain root: {err}"))
            })?,
        };

        let secret_files = find_secret_files(&scan_root, &self.policy.secret_guard_patterns);

        // Never surface the leaked text or file content in the error — only
        // the matched file's own basename (which is, by definition, one of
        // this node's own configured *patterns*, never secret material
        // itself).
        if let Some(matched) =
            text_contains_any_line_from(&research_content, &secret_files, MIN_LINE_LEN)
        {
            let basename = matched
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| matched.display().to_string());
            return Err(NodeError::new(format!(
                "{NODE_NAME}: research output appears to carry content lifted from a \
                 secret-shaped file ('{basename}') -- blocking notes.md persistence"
            )));
        }

        put_result(&mut ctx, NODE_NAME, serde_json::json!({CHECKED_KEY: true}));

        Ok(ctx)
    }

    fn name(&self) -> &str {
        NODE_NAME
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use serde_json::json;

    use super::*;

    fn empty_context(event: serde_json::Value) -> TaskContext {
        TaskContext {
            event,
            nodes: HashMap::new(),
            metadata: json!({}),
            node_runs: HashMap::new(),
        }
    }

    fn context_with_research(content: &str) -> TaskContext {
        let mut ctx = empty_context(json!({}));
        put_result(&mut ctx, research::NODE_NAME, json!({"content": content}));
        ctx
    }

    #[test]
    fn find_secret_files_returns_exactly_the_top_level_matches() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("normal.rs"), "fn main() {}").unwrap();
        std::fs::write(dir.path().join(".env"), "API_KEY=abc123\n").unwrap();
        std::fs::write(
            dir.path().join("credentials.json"),
            "{\"token\": \"abc123\"}",
        )
        .unwrap();

        let decoy_dir = dir.path().join("target");
        std::fs::create_dir_all(&decoy_dir).unwrap();
        std::fs::write(decoy_dir.join(".env"), "DECOY=1\n").unwrap();

        let patterns = default_patterns();
        let mut found = find_secret_files(dir.path(), &patterns);
        found.sort();

        let mut expected = vec![dir.path().join(".env"), dir.path().join("credentials.json")];
        expected.sort();

        assert_eq!(found, expected);
    }

    #[test]
    fn find_secret_files_prunes_every_declared_directory_name() {
        let dir = tempfile::tempdir().expect("tempdir");
        for pruned in PRUNED_DIR_NAMES {
            let sub = dir.path().join(pruned);
            std::fs::create_dir_all(&sub).unwrap();
            std::fs::write(sub.join(".env"), "PRUNED=1\n").unwrap();
        }

        let found = find_secret_files(dir.path(), &default_patterns());
        assert!(found.is_empty());
    }

    #[test]
    fn text_contains_any_line_from_returns_some_for_a_verbatim_line() {
        let dir = tempfile::tempdir().expect("tempdir");
        let needle = dir.path().join(".env");
        std::fs::write(&needle, "SUPER_SECRET_TOKEN=abcdef123456\n").unwrap();

        let haystack = "the research session found: SUPER_SECRET_TOKEN=abcdef123456 in a config";
        let result = text_contains_any_line_from(haystack, &[needle.clone()], MIN_LINE_LEN);
        assert_eq!(result, Some(needle));
    }

    #[test]
    fn text_contains_any_line_from_returns_none_for_a_paraphrase() {
        let dir = tempfile::tempdir().expect("tempdir");
        let needle = dir.path().join(".env");
        std::fs::write(&needle, "SUPER_SECRET_TOKEN=abcdef123456\n").unwrap();

        let haystack =
            "the research session mentioned there is some kind of secret token configured";
        let result = text_contains_any_line_from(haystack, &[needle], MIN_LINE_LEN);
        assert_eq!(result, None);
    }

    #[test]
    fn text_contains_any_line_from_returns_none_for_empty_needle_list() {
        let result = text_contains_any_line_from("anything at all here", &[], MIN_LINE_LEN);
        assert_eq!(result, None);
    }

    #[tokio::test]
    async fn process_on_leaked_content_errors_naming_only_the_matched_filename() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join(".env"), "SUPER_SECRET_TOKEN=abcdef123456\n").unwrap();

        let policy = PrePlanPolicy {
            secret_guard_scan_root: Some(dir.path().display().to_string()),
            ..PrePlanPolicy::default()
        };
        let node = SecretGuardNode::new(policy);
        let ctx = context_with_research(
            "notes: the session read a config with SUPER_SECRET_TOKEN=abcdef123456 in it",
        );

        let err = node
            .process(ctx)
            .await
            .expect_err("leaked content should be blocked");
        assert!(err.message.contains(".env"));
        assert!(!err.message.contains("SUPER_SECRET_TOKEN=abcdef123456"));
    }

    #[tokio::test]
    async fn process_on_clean_content_passes_through_unchanged() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join(".env"), "SUPER_SECRET_TOKEN=abcdef123456\n").unwrap();

        let policy = PrePlanPolicy {
            secret_guard_scan_root: Some(dir.path().display().to_string()),
            ..PrePlanPolicy::default()
        };
        let node = SecretGuardNode::new(policy);
        let research_content = "VERIFIED -- the widget module already exists in crates/foo.";
        let ctx = context_with_research(research_content);

        let ctx = node
            .process(ctx)
            .await
            .expect("clean content should pass through");
        let stored = ctx.nodes.get(NODE_NAME).expect("result stored");
        assert_eq!(
            stored.get(CHECKED_KEY).and_then(|v| v.as_bool()),
            Some(true)
        );
        assert_eq!(
            ctx.nodes[research::NODE_NAME]["content"].as_str(),
            Some(research_content)
        );
    }

    #[tokio::test]
    async fn process_errors_when_research_has_not_run() {
        let node = SecretGuardNode::default();
        let ctx = empty_context(json!({}));

        let err = node
            .process(ctx)
            .await
            .expect_err("should fail without ResearchCodebaseNode having run");
        assert!(err.message.contains(research::NODE_NAME));
    }

    #[test]
    fn name_matches_node_name_const() {
        assert_eq!(SecretGuardNode::default().name(), NODE_NAME);
    }

    #[test]
    fn default_patterns_is_non_empty() {
        assert!(!default_patterns().is_empty());
    }
}
