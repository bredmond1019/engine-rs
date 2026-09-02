//! Regression guard for `EN.ticket.externalize-node-prompts` (D24): every stable-prompt
//! `const` under `crates/engine-core/src/workflows/` must be an `include_str!` definition,
//! never an inline Rust string literal. A literal hides the prompt text from the colocated
//! `prompts/<node>.md` convention — unreviewable as prose, invisible to a diff — which is the
//! exact defect this ticket moved sixteen consts to fix.
//!
//! Scope: any `const` whose name contains `PROMPT` and whose type is `&str` (or `&'static
//! str`), declared anywhere in a `.rs` file under `workflows/`. The check is syntactic, not a
//! full Rust parse: it locates the `=` that starts the const's initializer and inspects the
//! first non-whitespace token after it. `include_str!` passes; a `"` or `r#"` string-literal
//! opener fails and names the offending file and const.
//!
//! Proven able to fail: see `docs/workflows/README.md` and the task 4 log in
//! `planning/EN.ticket.externalize-node-prompts/` for the verbatim FAIL/PASS observation this
//! test was required to produce before being accepted (CLAUDE.md standing rule 11).

use std::fs;
use std::path::{Path, PathBuf};

use regex::Regex;

/// Resolve the `engine-core` crate's source root from `CARGO_MANIFEST_DIR` — set by cargo to
/// this crate's own `Cargo.toml` directory for every test binary built from it — never from a
/// relative path off the test process's cwd, which nextest does not guarantee is the crate root.
fn workflows_root() -> PathBuf {
    let manifest_dir =
        std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR must be set by cargo");
    Path::new(&manifest_dir).join("src").join("workflows")
}

/// Walk every `.rs` file under `root`, recursively.
fn collect_rs_files(root: &Path, out: &mut Vec<PathBuf>) {
    let entries =
        fs::read_dir(root).unwrap_or_else(|e| panic!("failed to read dir {}: {e}", root.display()));
    for entry in entries {
        let entry = entry.expect("failed to read dir entry");
        let path = entry.path();
        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

/// One `const ...PROMPT...: &str = <initializer>` violation: a const whose initializer is an
/// inline string literal rather than `include_str!`.
struct Violation {
    file: PathBuf,
    const_name: String,
}

/// Scan `contents` for `PROMPT`-named `&str` consts whose initializer is not `include_str!`.
fn find_violations_in_source(file: &Path, contents: &str, const_decl: &Regex) -> Vec<Violation> {
    let mut violations = Vec::new();
    for caps in const_decl.captures_iter(contents) {
        let const_name = caps.name("name").unwrap().as_str();
        let rest = caps.name("rest").unwrap().as_str();
        let trimmed = rest.trim_start();
        let is_include_str = trimmed.starts_with("include_str!");
        if !is_include_str {
            violations.push(Violation {
                file: file.to_path_buf(),
                const_name: const_name.to_string(),
            });
        }
    }
    violations
}

/// Assert that every `PROMPT`-named `&str` const under `workflows/` is `include_str!`-backed.
///
/// This is the guard test itself — it must be observed FAILING against a deliberately
/// reintroduced inline literal (task 4 log) before being trusted, per CLAUDE.md standing rule
/// 11 ("a guard only ever seen green is not evidence of anything").
#[test]
fn all_stable_prompt_consts_are_include_str() {
    let root = workflows_root();
    assert!(
        root.is_dir(),
        "expected workflows root to exist at {}",
        root.display()
    );

    let mut rs_files = Vec::new();
    collect_rs_files(&root, &mut rs_files);
    assert!(
        !rs_files.is_empty(),
        "no .rs files found under {} — guard cannot have scanned anything",
        root.display()
    );

    // Matches `const <NAME>: &str = <rest up to the terminating `;`>` (also `&'static str`),
    // where <NAME> contains "PROMPT". `(?s)` lets `.` cross newlines so multi-line initializers
    // (e.g. a `format!`/concatenated literal spanning several lines) are captured whole; the
    // non-greedy `.*?` plus lookahead for `;\s*\n` stops at the first initializer-ending
    // semicolon rather than swallowing the rest of the file.
    let const_decl = Regex::new(
        r#"(?s)const\s+(?P<name>\w*PROMPT\w*)\s*:\s*&(?:'static\s+)?str\s*=\s*(?P<rest>.*?);\s*\n"#,
    )
    .expect("const_decl regex must compile");

    let mut violations = Vec::new();
    for file in &rs_files {
        let contents = fs::read_to_string(file)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", file.display()));
        violations.extend(find_violations_in_source(file, &contents, &const_decl));
    }

    assert!(
        violations.is_empty(),
        "found {} inline PROMPT const literal(s) under {} — every stable-prompt const must be \
         an include_str! definition (D24, CLAUDE.md standing rule 12):\n{}",
        violations.len(),
        root.display(),
        violations
            .iter()
            .map(|v| format!("  {} :: {}", v.file.display(), v.const_name))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// Prove the guard's detection logic actually fires on an inline literal, without touching any
/// real source file. Complements (does not replace) the manual FAIL/PASS observation against a
/// deliberately reintroduced real literal recorded in the task 4 log.
#[test]
fn guard_detects_inline_literal_in_synthetic_source() {
    let const_decl = Regex::new(
        r#"(?s)const\s+(?P<name>\w*PROMPT\w*)\s*:\s*&(?:'static\s+)?str\s*=\s*(?P<rest>.*?);\s*\n"#,
    )
    .expect("const_decl regex must compile");

    let inline = "const STABLE_SYSTEM_PROMPT: &str = \"hello world\";\n";
    let violations = find_violations_in_source(Path::new("synthetic.rs"), inline, &const_decl);
    assert_eq!(
        violations.len(),
        1,
        "guard must flag an inline PROMPT literal"
    );
    assert_eq!(violations[0].const_name, "STABLE_SYSTEM_PROMPT");

    let externalized = "const STABLE_SYSTEM_PROMPT: &str = include_str!(\"prompts/x.md\");\n";
    let violations =
        find_violations_in_source(Path::new("synthetic.rs"), externalized, &const_decl);
    assert!(
        violations.is_empty(),
        "guard must not flag an include_str! definition"
    );
}
