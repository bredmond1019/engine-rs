// detect/manifest.rs — manifest schema, compile, region resolver, gate matcher.
//
// Per-agent TOML manifests are deserialized into `Manifest` / `RuleSpec` /
// `GateSpec`, then compiled into `CompiledManifest` which pre-compiles every
// regex and sorts rules by descending priority. The compiled form is what
// `detect()` in `mod.rs` operates on.

use crate::detect::AgentState;
use regex::Regex;
use serde::Deserialize;

// ── Region selector ───────────────────────────────────────────────────────────

/// Which slice of the captured pane screen a rule inspects.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum RegionSpec {
    /// The entire screen (default when `region` is omitted).
    #[default]
    Whole,
    /// The final `n` lines, joined with `\n`. Falls back to the whole screen
    /// when the screen has fewer than `n` lines.
    LastLines { n: usize },
}

/// Resolve a region selector against a captured screen string. Pure — no I/O.
pub fn resolve_region(screen: &str, region: &RegionSpec) -> String {
    match region {
        RegionSpec::Whole => screen.to_string(),
        RegionSpec::LastLines { n } => {
            let lines: Vec<&str> = screen.lines().collect();
            if lines.len() <= *n {
                screen.to_string()
            } else {
                lines[lines.len() - n..].join("\n")
            }
        }
    }
}

// ── Gate spec (deserialized form) ─────────────────────────────────────────────

/// A matcher leaf or boolean combinator over child gates.
///
/// TOML examples:
/// - `gate = { contains = "Do you want to proceed?" }`
/// - `gate = { regex = "esc to interrupt" }`
/// - `gate = { line_regex = "^>" }`
/// - `gate = { any = [{ contains = "a" }, { contains = "b" }] }`
/// - `gate = { all = [{ contains = "a" }, { not = { contains = "b" } }] }`
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateSpec {
    /// Matches if the region contains the substring (case-sensitive).
    Contains(String),
    /// Matches if the compiled regex matches anywhere in the region.
    Regex(String),
    /// Matches if the compiled regex matches any single line of the region.
    LineRegex(String),
    /// Matches if any child gate matches (OR).
    Any(Vec<GateSpec>),
    /// Matches if all child gates match (AND).
    All(Vec<GateSpec>),
    /// Matches if the child gate does NOT match (NOT).
    Not(Box<GateSpec>),
}

// ── Rule spec (deserialized form) ─────────────────────────────────────────────

/// A single detection rule inside a manifest.
#[derive(Debug, Clone, Deserialize)]
pub struct RuleSpec {
    /// Screen region this rule inspects. Defaults to `Whole`.
    #[serde(default)]
    pub region: RegionSpec,
    /// The matcher expression (leaf or combinator tree).
    pub gate: GateSpec,
    /// Higher priority rules are evaluated first. Default 0.
    #[serde(default)]
    pub priority: i32,
    /// The agent state to report when this rule matches.
    pub state: AgentState,
    /// Carry-through visibility flags for the UI layer.
    #[serde(default)]
    pub visible_idle: bool,
    #[serde(default)]
    pub visible_blocker: bool,
    #[serde(default)]
    pub visible_working: bool,
    /// When true, the caller should not update the stored state record.
    #[serde(default)]
    pub skip_state_update: bool,
}

// ── Manifest (deserialized form) ──────────────────────────────────────────────

/// Top-level manifest, one per agent (e.g. `claude.toml`, `pi.toml`).
#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    /// Human-readable agent name (`"claude"`, `"pi"`, etc.).
    pub name: String,
    /// Detection rules. Empty list → every screen → `Unknown`.
    #[serde(default)]
    pub rules: Vec<RuleSpec>,
}

// ── Typed error ───────────────────────────────────────────────────────────────

/// Errors that can occur when parsing or compiling a manifest.
#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("manifest TOML parse error: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("invalid regex in manifest: {0}")]
    Regex(#[from] regex::Error),
}

// ── Compiled gate (regexes pre-compiled) ─────────────────────────────────────

/// Gate with all `Regex` / `LineRegex` patterns compiled.
pub enum CompiledGate {
    Contains(String),
    Regex(Regex),
    LineRegex(Regex),
    Any(Vec<CompiledGate>),
    All(Vec<CompiledGate>),
    Not(Box<CompiledGate>),
}

impl CompiledGate {
    /// Evaluate this gate against a resolved region string. Pure — no I/O.
    pub fn eval(&self, region: &str) -> bool {
        match self {
            CompiledGate::Contains(s) => region.contains(s.as_str()),
            CompiledGate::Regex(re) => re.is_match(region),
            CompiledGate::LineRegex(re) => region.lines().any(|l| re.is_match(l)),
            CompiledGate::Any(children) => children.iter().any(|g| g.eval(region)),
            CompiledGate::All(children) => children.iter().all(|g| g.eval(region)),
            CompiledGate::Not(child) => !child.eval(region),
        }
    }
}

/// Recursively compile a `GateSpec` into a `CompiledGate`.
fn compile_gate(spec: &GateSpec) -> Result<CompiledGate, ManifestError> {
    match spec {
        GateSpec::Contains(s) => Ok(CompiledGate::Contains(s.clone())),
        GateSpec::Regex(pat) => Ok(CompiledGate::Regex(Regex::new(pat)?)),
        GateSpec::LineRegex(pat) => Ok(CompiledGate::LineRegex(Regex::new(pat)?)),
        GateSpec::Any(children) => {
            let compiled: Result<Vec<_>, _> = children.iter().map(compile_gate).collect();
            Ok(CompiledGate::Any(compiled?))
        }
        GateSpec::All(children) => {
            let compiled: Result<Vec<_>, _> = children.iter().map(compile_gate).collect();
            Ok(CompiledGate::All(compiled?))
        }
        GateSpec::Not(child) => Ok(CompiledGate::Not(Box::new(compile_gate(child)?))),
    }
}

// ── Compiled rule ─────────────────────────────────────────────────────────────

/// A rule with its gate pre-compiled and ready for evaluation.
pub struct CompiledRule {
    pub region: RegionSpec,
    pub gate: CompiledGate,
    pub state: AgentState,
    pub visible_idle: bool,
    pub visible_blocker: bool,
    pub visible_working: bool,
    pub skip_state_update: bool,
}

// ── Compiled manifest ─────────────────────────────────────────────────────────

/// Manifest with all rules compiled and sorted in descending priority order.
pub struct CompiledManifest {
    pub name: String,
    /// Rules sorted by descending priority (stable: source order preserved on ties).
    pub rules: Vec<CompiledRule>,
}

// ── Parse + compile ───────────────────────────────────────────────────────────

/// Parse a TOML source string into a `Manifest`.
pub fn parse_manifest(toml_src: &str) -> Result<Manifest, ManifestError> {
    toml::from_str(toml_src).map_err(ManifestError::Toml)
}

impl Manifest {
    /// Compile this manifest: precompile all regex patterns and sort rules
    /// by descending priority (stable sort; source order preserved on ties).
    pub fn compile(self) -> Result<CompiledManifest, ManifestError> {
        let mut rules: Vec<(i32, CompiledRule)> = self
            .rules
            .iter()
            .map(|r| {
                let gate = compile_gate(&r.gate)?;
                Ok((
                    r.priority,
                    CompiledRule {
                        region: r.region.clone(),
                        gate,
                        state: r.state,
                        visible_idle: r.visible_idle,
                        visible_blocker: r.visible_blocker,
                        visible_working: r.visible_working,
                        skip_state_update: r.skip_state_update,
                    },
                ))
            })
            .collect::<Result<Vec<_>, ManifestError>>()?;

        // Stable descending sort: higher priority rules first.
        rules.sort_by_key(|b| std::cmp::Reverse(b.0));

        Ok(CompiledManifest {
            name: self.name,
            rules: rules.into_iter().map(|(_, r)| r).collect(),
        })
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Helper: compile a gate from an inline TOML rule fragment ─────────────

    /// Build a `CompiledGate` from a TOML `gate = …` value for test convenience.
    fn make_gate(gate_toml: &str) -> CompiledGate {
        // Directly deserialize just the GateSpec from a `gate = …` TOML fragment.
        let gate_src = format!("gate = {gate_toml}");
        #[derive(serde::Deserialize)]
        struct Wrapper {
            gate: GateSpec,
        }
        let w: Wrapper = toml::from_str(&gate_src).expect("gate TOML parse failed");
        compile_gate(&w.gate).expect("compile_gate failed")
    }

    // ── Contains matcher ──────────────────────────────────────────────────────

    #[test]
    fn contains_matches() {
        let gate = make_gate(r#"{ contains = "hello" }"#);
        assert!(gate.eval("say hello world"));
    }

    #[test]
    fn contains_no_match() {
        let gate = make_gate(r#"{ contains = "hello" }"#);
        assert!(!gate.eval("goodbye world"));
    }

    // ── Regex matcher ─────────────────────────────────────────────────────────

    #[test]
    fn regex_matches() {
        let gate = make_gate(r#"{ regex = "hel+o" }"#);
        assert!(gate.eval("say hello there"));
    }

    #[test]
    fn regex_no_match() {
        let gate = make_gate(r#"{ regex = "hel+o" }"#);
        assert!(!gate.eval("goodbye world"));
    }

    // ── LineRegex matcher ─────────────────────────────────────────────────────

    #[test]
    fn line_regex_matches_one_line() {
        let gate = make_gate(r#"{ line_regex = "^>" }"#);
        let screen = "normal line\n> prompt line\nanother line";
        assert!(gate.eval(screen));
    }

    #[test]
    fn line_regex_no_line_matches() {
        let gate = make_gate(r#"{ line_regex = "^>" }"#);
        let screen = "normal line\nno prompt here\nanother line";
        assert!(!gate.eval(screen));
    }

    // ── Any combinator ────────────────────────────────────────────────────────

    #[test]
    fn any_true_when_one_child_true() {
        let gate = make_gate(r#"{ any = [{ contains = "nope" }, { contains = "yes" }] }"#);
        assert!(gate.eval("yes it is here"));
    }

    #[test]
    fn any_false_when_no_child_true() {
        let gate = make_gate(r#"{ any = [{ contains = "nope" }, { contains = "neither" }] }"#);
        assert!(!gate.eval("something else entirely"));
    }

    // ── All combinator ────────────────────────────────────────────────────────

    #[test]
    fn all_true_when_all_children_true() {
        let gate = make_gate(r#"{ all = [{ contains = "hello" }, { contains = "world" }] }"#);
        assert!(gate.eval("hello world"));
    }

    #[test]
    fn all_false_when_one_child_false() {
        let gate = make_gate(r#"{ all = [{ contains = "hello" }, { contains = "world" }] }"#);
        assert!(!gate.eval("hello there"));
    }

    // ── Not combinator ────────────────────────────────────────────────────────

    #[test]
    fn not_negates_true_to_false() {
        let gate = make_gate(r#"{ not = { contains = "hello" } }"#);
        assert!(!gate.eval("hello world"));
    }

    #[test]
    fn not_negates_false_to_true() {
        let gate = make_gate(r#"{ not = { contains = "hello" } }"#);
        assert!(gate.eval("goodbye world"));
    }

    // ── Nested gate ───────────────────────────────────────────────────────────

    #[test]
    fn nested_gate_all_contains_not_any_regex() {
        // all(contains("alpha"), not(contains("beta")), any(regex("c+"), line_regex("^d")))
        let gate = make_gate(
            r#"{ all = [
                { contains = "alpha" },
                { not = { contains = "beta" } },
                { any = [{ regex = "c+" }, { line_regex = "^d" }] }
            ] }"#,
        );
        // "alpha" present, "beta" absent, regex "c+" matches "ccc"
        assert!(gate.eval("alpha ccc\nnormal line"));
        // "beta" present → all fails
        assert!(!gate.eval("alpha beta ccc"));
        // no "c+" and no line starting with "d" → any fails → all fails
        assert!(!gate.eval("alpha\nnormal"));
        // line starts with "d" → any passes
        assert!(gate.eval("alpha\ndstarts"));
    }

    // ── Region selectors ──────────────────────────────────────────────────────

    #[test]
    fn region_whole_returns_full_screen() {
        let screen = "line1\nline2\nline3";
        assert_eq!(resolve_region(screen, &RegionSpec::Whole), screen);
    }

    #[test]
    fn region_last_lines_returns_tail() {
        let screen = "line1\nline2\nline3\nline4\nline5";
        let result = resolve_region(screen, &RegionSpec::LastLines { n: 2 });
        assert_eq!(result, "line4\nline5");
    }

    #[test]
    fn region_last_lines_fewer_lines_than_n_returns_whole() {
        let screen = "only line";
        let result = resolve_region(screen, &RegionSpec::LastLines { n: 5 });
        assert_eq!(result, screen);
    }

    #[test]
    fn region_last_lines_exact_n_returns_whole() {
        let screen = "line1\nline2\nline3";
        let result = resolve_region(screen, &RegionSpec::LastLines { n: 3 });
        assert_eq!(result, screen);
    }

    // ── Priority ordering ─────────────────────────────────────────────────────

    #[test]
    fn compile_sorts_rules_descending_priority() {
        let src = r#"
name = "test"

[[rules]]
state = "idle"
priority = 1
gate = { contains = "idle" }

[[rules]]
state = "working"
priority = 10
gate = { contains = "working" }
"#;
        let manifest = parse_manifest(src).expect("parse failed");
        let compiled = manifest.compile().expect("compile failed");
        // Higher priority rule (working, 10) should come first.
        assert_eq!(compiled.rules.len(), 2);
        assert_eq!(compiled.rules[0].state, AgentState::Working);
        assert_eq!(compiled.rules[1].state, AgentState::Idle);
    }

    // ── Error paths ───────────────────────────────────────────────────────────

    #[test]
    fn compile_bad_regex_is_error() {
        let src = r#"
name = "test"

[[rules]]
state = "idle"
gate = { regex = "(" }
"#;
        let manifest = parse_manifest(src).expect("parse succeeded (TOML is valid)");
        let result = manifest.compile();
        assert!(
            matches!(result, Err(ManifestError::Regex(_))),
            "expected Regex error, got: {:?}",
            result.err()
        );
    }

    #[test]
    fn compile_bad_line_regex_is_error() {
        let src = r#"
name = "test"

[[rules]]
state = "idle"
gate = { line_regex = "[invalid" }
"#;
        let manifest = parse_manifest(src).expect("parse succeeded");
        let result = manifest.compile();
        assert!(matches!(result, Err(ManifestError::Regex(_))));
    }

    #[test]
    fn parse_malformed_toml_is_error() {
        let src = "name = \"test\"\n[[rules\nbroken toml";
        let result = parse_manifest(src);
        assert!(
            matches!(result, Err(ManifestError::Toml(_))),
            "expected Toml error, got: {:?}",
            result.err()
        );
    }

    #[test]
    fn parse_missing_required_state_is_error() {
        // `state` is required (no serde default); omitting it should fail to parse.
        let src = r#"
name = "test"

[[rules]]
priority = 1
gate = { contains = "x" }
"#;
        let result = parse_manifest(src);
        assert!(
            matches!(result, Err(ManifestError::Toml(_))),
            "expected Toml error for missing state field, got: {:?}",
            result.err()
        );
    }

    // ── skip_state_update flag carry-through ──────────────────────────────────

    #[test]
    fn compiled_rule_carries_skip_state_update() {
        let src = r#"
name = "test"

[[rules]]
state = "working"
skip_state_update = true
gate = { contains = "x" }
"#;
        let manifest = parse_manifest(src).expect("parse failed");
        let compiled = manifest.compile().expect("compile failed");
        assert!(compiled.rules[0].skip_state_update);
    }
}
