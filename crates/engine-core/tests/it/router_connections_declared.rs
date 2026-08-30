//! Every target a router's `route()` can return must appear in that
//! router's DECLARED `NodeConfig::connections`.
//!
//! **Why this file exists.** Nothing enforces declared connections at walk
//! time — `Workflow` happily dispatches to any registered node a router
//! names (that is D42's declared-acyclic / runtime-cyclic contract, and it
//! is deliberate). The consequence is that an undeclared routing arm is
//! invisible: runs keep working while the declared graph — what
//! `Workflow::new_validated` validates, what `GET /workflows/{type}/graph`
//! publishes, and what any reachability analysis over the schema reads — is
//! silently wrong. That is exactly what happened to
//! `sdlc_flow::TriageRouterNode`, whose `PASS` arm returns
//! `UpdateTaskStatusNode` under `ReviewMode::EndOnly` and under
//! `ReviewMode::TrivialSkip`-with-a-trivial-diff, while `graph.rs` declared
//! only `{ConsolidatedReviewNode, IncrementAttemptNode, WrapUpNode}` — so a
//! reachability analysis run against the schema reported a node as having
//! no inbound route from that router when it plainly does.
//!
//! **How the check works, and its honest limits.** A router's possible
//! targets are computed inside `route()` and are not mechanically
//! enumerable at runtime — there is no way to ask a `dyn Router` "what
//! could you return?" without driving it through every reachable context.
//! So this file does the next-best mechanical thing: it reads each router's
//! `impl Router for <Name>` block out of its own SOURCE FILE via
//! `include_str!` and extracts every `Some("...".to_string())` literal.
//! That is a lexical check, and it inherits lexical limits:
//!
//! * It only sees targets written as a literal in that exact shape. A
//!   target built by `format!`, read from a constant, or returned by a
//!   helper function would be missed.
//! * It only reads the router impl's own body, not helpers it calls.
//! * It cannot tell a reachable arm from a dead one, so it is a superset
//!   check: over-declaring a connection is not flagged.
//!
//! It nonetheless catches the whole class that actually bites here — an
//! edit that adds a new `Some("SomeNode".to_string())` arm without touching
//! `graph.rs`. To keep the lexical extractor itself honest (a parse that
//! silently truncated a body would let an undeclared arm slip through as a
//! clean pass), every router additionally carries a HAND-WRITTEN expected
//! target set that the extractor's output must match exactly. If the
//! extractor breaks, that assertion fails loudly rather than passing empty.
//!
//! Deliberately NOT done here: making `Workflow::new_validated` reject a
//! graph whose router can route outside its declared connections. That
//! would be a runtime behaviour change affecting every workflow in the
//! repo (and would break D42's legitimate runtime back-edges), so it is an
//! operator decision, not a side effect of this test.

use std::collections::BTreeSet;

use engine_core::WorkflowSchema;

// --- the source files each router impl lives in -----------------------------

const SDLC_FLOW_TASK_LOOP: &str = include_str!("../../src/workflows/sdlc_flow/task_loop.rs");
const SDLC_FLOW_SETUP: &str = include_str!("../../src/workflows/sdlc_flow/setup.rs");
const SDLC_FLOW_END_REVIEW: &str = include_str!("../../src/workflows/sdlc_flow/end_review.rs");
const SDLC_TASK_TRIAGE_ROUTER: &str =
    include_str!("../../src/workflows/sdlc_task/task_triage_router.rs");

/// One router under check: its node identity, the source text of the file
/// its `impl Router` block lives in, and the hand-written set of targets
/// that block is expected to be able to return.
struct RouterUnderCheck {
    identity: &'static str,
    source: &'static str,
    expected_targets: &'static [&'static str],
}

/// Every router reachable from the `SDLC_FLOW` declared graph.
fn sdlc_flow_routers() -> Vec<RouterUnderCheck> {
    vec![
        RouterUnderCheck {
            identity: "SpecExistsRouterNode",
            source: SDLC_FLOW_SETUP,
            expected_targets: &["GenerateTasksNode", "LoadTaskStateNode"],
        },
        RouterUnderCheck {
            identity: "TaskQueueRouterNode",
            source: SDLC_FLOW_TASK_LOOP,
            expected_targets: &["FinalValidationNode", "ImplementTaskNode"],
        },
        RouterUnderCheck {
            identity: "TriageRouterNode",
            source: SDLC_FLOW_TASK_LOOP,
            expected_targets: &[
                "ConsolidatedReviewNode",
                "IncrementAttemptNode",
                // The arm this whole file was written for.
                "UpdateTaskStatusNode",
                "WrapUpNode",
            ],
        },
        RouterUnderCheck {
            identity: "ReviewRouterNode",
            source: SDLC_FLOW_TASK_LOOP,
            expected_targets: &["IncrementAttemptNode", "UpdateTaskStatusNode", "WrapUpNode"],
        },
        RouterUnderCheck {
            identity: "EndReviewRouterNode",
            source: SDLC_FLOW_END_REVIEW,
            expected_targets: &["PatchDocsNode", "WrapUpNode"],
        },
    ]
}

/// Every router reachable from the `SDLC_TASK` declared graph.
/// `SpecExistsRouterNode` and `TaskQueueRouterNode` are the SAME `sdlc_flow`
/// types, reused unmodified (see `sdlc_task/graph.rs`), so they are checked
/// again here against `SDLC_TASK`'s own declared connections.
fn sdlc_task_routers() -> Vec<RouterUnderCheck> {
    vec![
        RouterUnderCheck {
            identity: "SpecExistsRouterNode",
            source: SDLC_FLOW_SETUP,
            expected_targets: &["GenerateTasksNode", "LoadTaskStateNode"],
        },
        RouterUnderCheck {
            identity: "TaskQueueRouterNode",
            source: SDLC_FLOW_TASK_LOOP,
            expected_targets: &["FinalValidationNode", "ImplementTaskNode"],
        },
        RouterUnderCheck {
            identity: "TaskTriageRouterNode",
            source: SDLC_TASK_TRIAGE_ROUTER,
            expected_targets: &[
                "IncrementAttemptNode",
                "LeanBookkeepNode",
                "UpdateTaskStatusNode",
            ],
        },
    ]
}

// --- the lexical extractor --------------------------------------------------

/// The body of `impl Router for <identity> { ... }` in `source`, brace-
/// balanced, with `//` line comments and string literals skipped so a brace
/// inside either cannot unbalance the scan. Panics if the block is absent
/// or unbalanced — a silent miss here would turn this whole file into a
/// vacuous pass.
fn router_impl_body(source: &str, identity: &str) -> String {
    let header = format!("impl Router for {identity} ");
    let start = source
        .find(&header)
        .unwrap_or_else(|| panic!("no `{header}{{` block found in source"));
    let rest = &source[start + header.len()..];
    let open = rest
        .find('{')
        .unwrap_or_else(|| panic!("`{header}` is not followed by `{{`"));

    let bytes: Vec<char> = rest[open..].chars().collect();
    let mut depth = 0usize;
    let mut i = 0usize;
    let mut in_string = false;
    while i < bytes.len() {
        let c = bytes[i];
        if in_string {
            if c == '\\' {
                i += 2;
                continue;
            }
            if c == '"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        match c {
            '"' => in_string = true,
            '/' if bytes.get(i + 1) == Some(&'/') => {
                while i < bytes.len() && bytes[i] != '\n' {
                    i += 1;
                }
                continue;
            }
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return bytes[..=i].iter().collect();
                }
            }
            _ => {}
        }
        i += 1;
    }
    panic!("unbalanced braces scanning `impl Router for {identity}`");
}

/// Every `Some("X".to_string())` target literal in a router impl body.
///
/// Deliberately anchored on the full `Some("` … `".to_string())` shape:
/// bare `"SomeNode"` literals inside a body are usually READS of an
/// upstream node's result (`get_result(ctx, "TriageTaskNode")`), not routing
/// targets, and matching those would produce false positives.
fn declared_route_targets(source: &str, identity: &str) -> BTreeSet<String> {
    let body = router_impl_body(source, identity);
    let mut out = BTreeSet::new();
    let open = "Some(\"";
    let close = "\".to_string())";
    let mut rest = body.as_str();
    while let Some(idx) = rest.find(open) {
        let after = &rest[idx + open.len()..];
        if let Some(end) = after.find(close) {
            let candidate = &after[..end];
            if !candidate.contains('"') {
                out.insert(candidate.to_string());
            }
            rest = &after[end + close.len()..];
        } else {
            rest = after;
        }
    }
    out
}

// --- the check ---------------------------------------------------------------

fn assert_routers_declared(schema: &WorkflowSchema, routers: &[RouterUnderCheck], label: &str) {
    for router in routers {
        let config = schema.nodes.get(router.identity).unwrap_or_else(|| {
            panic!("{label}: router '{}' is not in the schema", router.identity)
        });
        let declared: BTreeSet<String> = config.connections.iter().cloned().collect();

        // 1. The extractor still works: what it found matches the
        //    hand-written expectation exactly. Guards against a lexical
        //    parse that silently returned a truncated (or empty) set.
        let found = declared_route_targets(router.source, router.identity);
        let expected: BTreeSet<String> = router
            .expected_targets
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        assert_eq!(
            found, expected,
            "{label}: source scan of `{}::route` found targets {found:?}, but this test \
             expects {expected:?}. If you added or removed a routing arm, update \
             `expected_targets` AND the router's declared connections in graph.rs.",
            router.identity
        );

        // 2. The real invariant: every target the router can return is a
        //    declared connection.
        for target in &found {
            assert!(
                declared.contains(target),
                "{label}: `{}::route` can return \"{target}\", but it is not in that node's \
                 declared connections {declared:?} in graph.rs. Add it — the declared graph is \
                 what `Workflow::new_validated` validates, what `GET /workflows/{{type}}/graph` \
                 publishes, and what reachability analysis reads.",
                router.identity
            );
        }
    }
}

#[test]
fn sdlc_flow_router_targets_are_all_declared_connections() {
    let schema = engine_core::workflows::sdlc_flow::graph::schema();
    assert_routers_declared(&schema, &sdlc_flow_routers(), "SDLC_FLOW");
}

#[test]
fn sdlc_task_router_targets_are_all_declared_connections() {
    let schema = engine_core::workflows::sdlc_task::graph::schema();
    assert_routers_declared(&schema, &sdlc_task_routers(), "SDLC_TASK");
}

/// The specific regression: `TriageRouterNode`'s review-skipping `PASS`
/// target is declared. Asserted on the assembled schema, not on prose.
#[test]
fn triage_router_declares_update_task_status_node() {
    let schema = engine_core::workflows::sdlc_flow::graph::schema();
    let connections = &schema.nodes["TriageRouterNode"].connections;
    for target in [
        "ConsolidatedReviewNode",
        "UpdateTaskStatusNode",
        "IncrementAttemptNode",
        "WrapUpNode",
    ] {
        assert!(
            connections.contains(&target.to_string()),
            "TriageRouterNode must declare '{target}'; declared: {connections:?}"
        );
    }
}

// --- positive controls for the detector -------------------------------------

/// The extractor really does see the arm that was missing. Without this,
/// an empty or truncated scan would make every assertion above vacuous.
#[test]
fn extractor_finds_the_known_bad_triage_router_arm() {
    let found = declared_route_targets(SDLC_FLOW_TASK_LOOP, "TriageRouterNode");
    assert!(
        found.contains("UpdateTaskStatusNode"),
        "extractor failed to find `TriageRouterNode`'s `UpdateTaskStatusNode` arm; found {found:?}"
    );
    // …and it does NOT pick up the node names this body merely READS
    // (`get_result(ctx, \"TriageTaskNode\")`), which would be a false positive.
    assert!(
        !found.contains("TriageTaskNode"),
        "extractor picked up a result-read node name as a routing target: {found:?}"
    );
}

/// The check FAILS on a graph that omits a returnable target — proof the
/// assertion has teeth, run against the exact pre-fix declaration.
#[test]
fn check_rejects_the_pre_fix_triage_router_declaration() {
    let mut schema = engine_core::workflows::sdlc_flow::graph::schema();
    // Restore the defective declaration this ticket fixed.
    schema
        .nodes
        .get_mut("TriageRouterNode")
        .expect("TriageRouterNode in schema")
        .connections = vec![
        "ConsolidatedReviewNode".to_string(),
        "IncrementAttemptNode".to_string(),
        "WrapUpNode".to_string(),
    ];

    let routers = sdlc_flow_routers();
    let result = std::panic::catch_unwind(move || {
        assert_routers_declared(&schema, &routers, "SDLC_FLOW(pre-fix)");
    });
    assert!(
        result.is_err(),
        "the pre-fix TriageRouterNode declaration must be rejected by this check"
    );
}

/// The extractor sees a NEW undeclared arm in a router body — the future
/// edit this file exists to catch.
#[test]
fn extractor_sees_a_newly_added_arm() {
    let synthetic = r#"
impl Router for FakeRouterNode {
    fn route(&self, ctx: &TaskContext) -> Option<String> {
        let v = get_result(ctx, "SomeUpstreamNode")?;
        match v {
            "A" => Some("DeclaredNode".to_string()),
            // a brace in a comment { must not unbalance the scan
            _ => Some("BrandNewUndeclaredNode".to_string()),
        }
    }
}
"#;
    let found = declared_route_targets(synthetic, "FakeRouterNode");
    assert_eq!(
        found,
        ["BrandNewUndeclaredNode", "DeclaredNode"]
            .iter()
            .map(|s| (*s).to_string())
            .collect::<BTreeSet<String>>()
    );
}

// --- completeness -----------------------------------------------------------

/// The router tables above cover EVERY router in each declared graph.
///
/// Without this, a future router added to a workflow would simply not be
/// checked, and the suite would stay green — the survey's blind spot rather
/// than the routers' bug. Membership is decided by the registry's own
/// `as_router()`, the same predicate `WorkflowValidator` uses, not by a
/// hand-kept list.
fn assert_every_router_is_checked(
    schema: &WorkflowSchema,
    registry: &engine_core::NodeRegistry,
    routers: &[RouterUnderCheck],
    label: &str,
) {
    let checked: BTreeSet<&str> = routers.iter().map(|r| r.identity).collect();
    let actual: BTreeSet<&str> = schema
        .nodes
        .keys()
        .filter(|identity| {
            registry
                .get(identity)
                .is_some_and(|node| node.as_router().is_some())
        })
        .map(String::as_str)
        .collect();
    assert_eq!(
        checked, actual,
        "{label}: the set of routers this test checks must equal the set of routers actually \
         in the graph. Add the new router to the table in \
         tests/it/router_connections_declared.rs."
    );
}

#[test]
fn sdlc_flow_router_table_covers_every_router() {
    let schema = engine_core::workflows::sdlc_flow::graph::schema();
    let registry = engine_core::workflows::sdlc_flow::graph::registry();
    assert_every_router_is_checked(&schema, &registry, &sdlc_flow_routers(), "SDLC_FLOW");
}

#[test]
fn sdlc_task_router_table_covers_every_router() {
    let schema = engine_core::workflows::sdlc_task::graph::schema();
    let registry = engine_core::workflows::sdlc_task::graph::registry();
    assert_every_router_is_checked(&schema, &registry, &sdlc_task_routers(), "SDLC_TASK");
}
