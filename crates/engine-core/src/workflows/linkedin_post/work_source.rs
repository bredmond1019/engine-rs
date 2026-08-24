//! `WorkSourceNode` — reads the week's actual work (fleet git history,
//! `log.md` entries, new `planning/decisions/` files) for the event's date
//! range and emits `Vec<WorkSource>` into `ctx.nodes`. The only genuinely
//! new node in this block (`planning/EN.5.G/tasks.md` + `tasks.json`
//! task 2).
//!
//! Every subprocess invocation goes through the injectable
//! [`crate::workflows::CommandRunner`] seam — mirrors
//! `sdlc_flow::end_review::EndReviewNode::with_runner`. Every filesystem
//! read goes through a small injectable [`FileReader`]/[`DirReader`] seam
//! for the same reason: the gated `cargo nextest` suite must never shell
//! out to a real `git` or touch the real filesystem.
//!
//! An empty (or inverted) date range short-circuits before either seam is
//! called at all and yields an empty `Vec<WorkSource>` with a clear
//! `message` — never a fabricated source.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};

use engine_contract::TaskContext;
use regex::Regex;

use crate::node::{Node, NodeError};
use crate::workflows::{default_command_runner, put_result, CommandRunner};

use super::schema::{LinkedInPostEventSchema, WorkSource, WorkSourceKind};

/// The `Node::name()` identity `WorkSourceNode` is registered under, and
/// the `ctx.nodes` key its gathered sources are stamped onto.
pub const NODE_NAME: &str = "WorkSourceNode";

/// Reads a single file to a UTF-8 string — used for `log.md`. Defaults to
/// [`default_file_reader`]; tests substitute a stub so the gated suite
/// never touches the real filesystem, mirroring [`CommandRunner`].
pub type FileReader = Arc<dyn Fn(&Path) -> io::Result<String> + Send + Sync>;

/// Lists `(filename, content)` pairs for every regular file directly
/// inside a directory — used for `planning/decisions/`. Same rationale as
/// [`FileReader`].
pub type DirReader = Arc<dyn Fn(&Path) -> io::Result<Vec<(String, String)>> + Send + Sync>;

/// The real [`FileReader`]: `std::fs::read_to_string`.
#[must_use]
pub fn default_file_reader() -> FileReader {
    Arc::new(|path| std::fs::read_to_string(path))
}

/// The real [`DirReader`]: lists regular files directly inside `dir` and
/// reads each one. A missing directory (e.g. no `planning/decisions/` in
/// this repo) is reported as an `io::Error`, which [`WorkSourceNode`]
/// treats as "no decision sources" rather than a hard failure.
#[must_use]
pub fn default_dir_reader() -> DirReader {
    Arc::new(|dir| {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let content = std::fs::read_to_string(&path)?;
            out.push((name, content));
        }
        Ok(out)
    })
}

/// A four-digit-dash-two-digit-dash-two-digit ISO date, e.g. `2026-08-24`.
static ISO_DATE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\d{4}-\d{2}-\d{2}").expect("static regex is valid"));

/// The fleet's known repos, relative to `fleet_root` — used only as this
/// node's built-in default when the event's `repos` field is omitted.
/// Deliberately conservative (just this repo): this node has no seam onto
/// `brain.toml`'s `[[repos]]` table, and adding one to enumerate "the
/// whole fleet" accurately is out of this task's scope (see
/// `planning/EN.5.G/tasks.md` task 2). A caller that wants the full fleet
/// passes `repos` explicitly on the event.
const DEFAULT_REPOS: &[&str] = &["."];

/// Reads git history, `log.md`, and `planning/decisions/` for the event's
/// date range and emits `Vec<WorkSource>`.
pub struct WorkSourceNode {
    runner: CommandRunner,
    file_reader: FileReader,
    dir_reader: DirReader,
    /// Root the fleet is read relative to — `log.md` and
    /// `planning/decisions/` are resolved under here; each repo in the
    /// resolved `repos` list is a directory under here `git log` runs in.
    fleet_root: PathBuf,
}

impl WorkSourceNode {
    #[must_use]
    pub fn new() -> Self {
        Self {
            runner: default_command_runner(),
            file_reader: default_file_reader(),
            dir_reader: default_dir_reader(),
            fleet_root: PathBuf::from("."),
        }
    }

    /// Override the command runner used for `git log` invocations.
    #[must_use]
    pub fn with_runner(mut self, runner: CommandRunner) -> Self {
        self.runner = runner;
        self
    }

    /// Override the reader used for `log.md`.
    #[must_use]
    pub fn with_file_reader(mut self, reader: FileReader) -> Self {
        self.file_reader = reader;
        self
    }

    /// Override the reader used for `planning/decisions/`.
    #[must_use]
    pub fn with_dir_reader(mut self, reader: DirReader) -> Self {
        self.dir_reader = reader;
        self
    }

    /// Override the fleet root `log.md`/`planning/decisions/`/each repo
    /// are resolved under. Defaults to `.`.
    #[must_use]
    pub fn with_fleet_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.fleet_root = root.into();
        self
    }

    /// Gather commit sources across every repo in `repos` for `[since,
    /// until]`, via the injected `CommandRunner`.
    fn gather_commits(&self, repos: &[String], since: &str, until: &str) -> Vec<WorkSource> {
        let mut sources = Vec::new();
        for repo in repos {
            let repo_dir = self.fleet_root.join(repo);
            let args = [
                "log",
                "--since",
                since,
                "--until",
                until,
                "--format=%H\x1f%s",
            ];
            let Ok(output) = (self.runner)("git", &args, &repo_dir) else {
                continue;
            };
            if output.status != 0 {
                continue;
            }
            for line in output.stdout.lines() {
                let Some((hash, subject)) = line.split_once('\x1f') else {
                    continue;
                };
                if hash.trim().is_empty() {
                    continue;
                }
                sources.push(WorkSource {
                    kind: WorkSourceKind::Commit,
                    id: hash.trim().to_string(),
                    summary: subject.trim().to_string(),
                });
            }
        }
        sources
    }

    /// Gather `log.md` entries in `[since, until]` via the injected
    /// `FileReader`. `log.md` groups entries under `## [YYYY-MM-DD]`
    /// headers, each containing one or more `### <title>` entries — see
    /// `agentic-portfolio/log.md`.
    fn gather_log_entries(&self, since: &str, until: &str) -> Vec<WorkSource> {
        let log_path = self.fleet_root.join("log.md");
        let Ok(content) = (self.file_reader)(&log_path) else {
            return Vec::new();
        };
        parse_log_entries(&content, since, until)
    }

    /// Gather `planning/decisions/` files whose recorded date falls in
    /// `[since, until]`, via the injected `DirReader`.
    fn gather_decisions(&self, since: &str, until: &str) -> Vec<WorkSource> {
        let decisions_dir = self.fleet_root.join("planning").join("decisions");
        let Ok(files) = (self.dir_reader)(&decisions_dir) else {
            return Vec::new();
        };
        parse_decisions(&files, since, until)
    }
}

impl Default for WorkSourceNode {
    fn default() -> Self {
        Self::new()
    }
}

/// Parse `## [DATE]` / `### title` structured entries out of `log.md`
/// content, keeping only entries whose section date falls in `[since,
/// until]` (inclusive, lexicographic — valid for ISO-8601 dates).
fn parse_log_entries(content: &str, since: &str, until: &str) -> Vec<WorkSource> {
    let mut out = Vec::new();
    let mut current_date: Option<String> = None;
    let mut seq = 0usize;

    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("## [") {
            if let Some(date) = rest.strip_suffix(']') {
                current_date = Some(date.trim().to_string());
                continue;
            }
        }
        if let Some(title) = trimmed.strip_prefix("### ") {
            let Some(date) = current_date.as_deref() else {
                continue;
            };
            if date >= since && date <= until {
                seq += 1;
                out.push(WorkSource {
                    kind: WorkSourceKind::LogEntry,
                    id: format!("{date}-{seq}"),
                    summary: title.trim().to_string(),
                });
            }
        }
    }
    out
}

/// Parse a `planning/decisions/` directory listing, keeping only files
/// whose recorded date (a `**Date:**` or `**Decided:**` line, per the
/// decisions template) falls in `[since, until]`.
fn parse_decisions(files: &[(String, String)], since: &str, until: &str) -> Vec<WorkSource> {
    let mut out = Vec::new();
    for (name, content) in files {
        if !name.ends_with(".md") {
            continue;
        }
        let Some(date) = extract_decision_date(content) else {
            continue;
        };
        if date.as_str() < since || date.as_str() > until {
            continue;
        }
        let doc_id = name.trim_end_matches(".md").to_string();
        let summary = extract_decision_title(content).unwrap_or_else(|| doc_id.clone());
        out.push(WorkSource {
            kind: WorkSourceKind::Decision,
            id: doc_id,
            summary,
        });
    }
    out
}

/// Find the first `**Date:**`/`**Decided:**` line and extract its
/// ISO-8601 date. Returns `None` (never a fabricated date) when no such
/// line exists or it carries no parseable date.
fn extract_decision_date(content: &str) -> Option<String> {
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("**Date:**") || trimmed.starts_with("**Decided:**") {
            if let Some(found) = ISO_DATE.find(trimmed) {
                return Some(found.as_str().to_string());
            }
        }
    }
    None
}

/// Extract the human-readable title from a decision's `title:` frontmatter
/// line, or its first `# ` heading if no frontmatter title is present.
fn extract_decision_title(content: &str) -> Option<String> {
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("title:") {
            let title = rest.trim().trim_matches('"');
            if !title.is_empty() {
                return Some(title.to_string());
            }
        }
    }
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("# ") {
            if !rest.trim().is_empty() {
                return Some(rest.trim().to_string());
            }
        }
    }
    None
}

#[async_trait::async_trait]
impl Node for WorkSourceNode {
    async fn process(&self, mut ctx: TaskContext) -> Result<TaskContext, NodeError> {
        let event: LinkedInPostEventSchema = serde_json::from_value(ctx.event.clone())
            .map_err(|err| NodeError::new(format!("WorkSourceNode: invalid event: {err}")))?;

        // Empty/inverted range: short-circuit before either seam is
        // touched at all, and emit a clear message rather than a
        // fabricated source.
        if event.since > event.until {
            put_result(
                &mut ctx,
                NODE_NAME,
                serde_json::json!({
                    "sources": Vec::<WorkSource>::new(),
                    "message": format!(
                        "empty date range: since ({}) is after until ({})",
                        event.since, event.until
                    ),
                }),
            );
            return Ok(ctx);
        }

        let repos = event
            .repos
            .clone()
            .unwrap_or_else(|| DEFAULT_REPOS.iter().map(|r| (*r).to_string()).collect());

        let mut sources = Vec::new();
        sources.extend(self.gather_commits(&repos, &event.since, &event.until));
        sources.extend(self.gather_log_entries(&event.since, &event.until));
        sources.extend(self.gather_decisions(&event.since, &event.until));

        let message = if sources.is_empty() {
            Some(format!(
                "no work found between {} and {}",
                event.since, event.until
            ))
        } else {
            None
        };

        put_result(
            &mut ctx,
            NODE_NAME,
            serde_json::json!({
                "sources": sources,
                "message": message,
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
    use crate::workflows::CommandOutput;

    fn ctx_for(event: serde_json::Value) -> TaskContext {
        TaskContext {
            event,
            nodes: HashMap::new(),
            metadata: json!({}),
            node_runs: HashMap::new(),
        }
    }

    fn event(since: &str, until: &str) -> serde_json::Value {
        json!({ "since": since, "until": until })
    }

    /// A runner that records every invocation's argv and returns a fixed
    /// `git log` stdout for any `git log` call.
    fn recording_runner(
        calls: Arc<Mutex<Vec<Vec<String>>>>,
        git_log_stdout: &'static str,
    ) -> CommandRunner {
        Arc::new(move |program, args, _cwd| {
            calls.lock().unwrap().push(
                std::iter::once(program.to_string())
                    .chain(args.iter().map(|a| a.to_string()))
                    .collect(),
            );
            Ok(CommandOutput {
                status: 0,
                stdout: git_log_stdout.to_string(),
                stderr: String::new(),
            })
        })
    }

    fn failing_reader() -> FileReader {
        Arc::new(|_path| Err(io::Error::new(io::ErrorKind::NotFound, "no such file")))
    }

    fn empty_dir_reader() -> DirReader {
        Arc::new(|_dir| Ok(Vec::new()))
    }

    fn unreachable_runner() -> CommandRunner {
        Arc::new(|_program, _args, _cwd| {
            panic!("runner must not be invoked for an empty date range")
        })
    }

    fn unreachable_file_reader() -> FileReader {
        Arc::new(|_path| panic!("file reader must not be invoked for an empty date range"))
    }

    fn unreachable_dir_reader() -> DirReader {
        Arc::new(|_dir| panic!("dir reader must not be invoked for an empty date range"))
    }

    #[tokio::test]
    async fn invokes_runner_with_since_until_in_git_log_argv() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let node = WorkSourceNode::new()
            .with_runner(recording_runner(calls.clone(), ""))
            .with_file_reader(failing_reader())
            .with_dir_reader(empty_dir_reader());

        node.process(ctx_for(event("2026-08-17", "2026-08-24")))
            .await
            .expect("process succeeds");

        let calls = calls.lock().unwrap();
        assert!(!calls.is_empty(), "runner should have been invoked");
        let git_log_call = calls
            .iter()
            .find(|call| call.first().map(String::as_str) == Some("git"))
            .expect("a git invocation happened");
        assert!(git_log_call.contains(&"log".to_string()));
        assert!(git_log_call.contains(&"--since".to_string()));
        assert!(git_log_call.contains(&"2026-08-17".to_string()));
        assert!(git_log_call.contains(&"--until".to_string()));
        assert!(git_log_call.contains(&"2026-08-24".to_string()));
    }

    #[tokio::test]
    async fn three_commits_plus_two_log_entries_yield_five_sources() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let git_log_stdout = "\
aaa1111\x1fShipped thing one
bbb2222\x1fShipped thing two
ccc3333\x1fShipped thing three
";
        let log_md = "\
## [2026-08-18]

### First entry title
- **What:** did a thing.

### Second entry title
- **What:** did another thing.

## [2026-09-01]

### Out of range entry
- **What:** should not appear.
";
        let file_reader: FileReader = Arc::new(move |_path| Ok(log_md.to_string()));

        let node = WorkSourceNode::new()
            .with_runner(recording_runner(calls, git_log_stdout))
            .with_file_reader(file_reader)
            .with_dir_reader(empty_dir_reader());

        let result = node
            .process(ctx_for(event("2026-08-17", "2026-08-24")))
            .await
            .expect("process succeeds");

        let stored = result.nodes.get(NODE_NAME).expect("result stored");
        let sources = stored["sources"].as_array().expect("sources array");
        assert_eq!(sources.len(), 5, "sources: {sources:#?}");

        let commit_count = sources
            .iter()
            .filter(|s| s["kind"] == json!("commit"))
            .count();
        let log_entry_count = sources
            .iter()
            .filter(|s| s["kind"] == json!("log-entry"))
            .count();
        assert_eq!(commit_count, 3);
        assert_eq!(log_entry_count, 2);
    }

    #[tokio::test]
    async fn empty_date_range_yields_empty_vec_and_never_touches_seams() {
        let node = WorkSourceNode::new()
            .with_runner(unreachable_runner())
            .with_file_reader(unreachable_file_reader())
            .with_dir_reader(unreachable_dir_reader());

        let result = node
            .process(ctx_for(event("2026-08-24", "2026-08-17")))
            .await
            .expect("process succeeds even for an inverted range");

        let stored = result.nodes.get(NODE_NAME).expect("result stored");
        let sources = stored["sources"].as_array().expect("sources array");
        assert!(sources.is_empty());
        assert!(stored["message"]
            .as_str()
            .unwrap()
            .contains("empty date range"));
    }

    #[test]
    fn parse_log_entries_respects_date_range() {
        let log_md = "\
## [2026-08-18]

### In range
- text

## [2026-09-01]

### Out of range
- text
";
        let out = parse_log_entries(log_md, "2026-08-17", "2026-08-24");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].summary, "In range");
        assert_eq!(out[0].kind, WorkSourceKind::LogEntry);
    }

    #[test]
    fn parse_decisions_extracts_date_and_title() {
        let content = "\
---
type: Decision
title: \"D99: A Decision\"
---

# D99 — A Decision

**Date:** 2026-08-20
";
        let files = vec![("D99-a-decision.md".to_string(), content.to_string())];
        let out = parse_decisions(&files, "2026-08-17", "2026-08-24");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, "D99-a-decision");
        assert_eq!(out[0].summary, "D99: A Decision");
        assert_eq!(out[0].kind, WorkSourceKind::Decision);
    }

    #[test]
    fn parse_decisions_skips_out_of_range_and_undated_files() {
        let dated_out_of_range = "\
**Date:** 2026-01-01
";
        let undated = "no date line here";
        let files = vec![
            (
                "D1-out-of-range.md".to_string(),
                dated_out_of_range.to_string(),
            ),
            ("D2-undated.md".to_string(), undated.to_string()),
        ];
        let out = parse_decisions(&files, "2026-08-17", "2026-08-24");
        assert!(out.is_empty());
    }
}
