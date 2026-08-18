//! Session identity for the Phase-2 terminal nodes (`EN.9.D` task 1).
//!
//! [`session_name_for`] is the pure, deterministic tmux session-name
//! derivation both `TerminalSessionNode` (task 2) and `TerminalObserveNode`
//! (task 4) resolve against: `eng-` namespaced, a function of `run_id` +
//! `node_identity` alone, with `:` and `.` sanitized out of every input
//! part before concatenation — tmux treats both characters as
//! session/window/pane target separators (`session:window.pane`), so an
//! unsanitized name would silently address the wrong target rather than
//! erroring.
//!
//! [`HasSessionInput`] / [`WithSessionInput`] give every terminal node the
//! same `session_input: InputBinding` field + `#[must_use]`,
//! self-consuming `with_session_input_from(self, upstream)` builder that
//! `revise.rs:59-69`'s `summary_input`/`critic_input` fields follow and the
//! existing `with_transport`/`with_http_post` builder convention
//! establishes — via one blanket impl here rather than hand-writing the
//! same three lines on every struct in this module. A node stores
//! `session_input: InputBinding` as a field, implements
//! [`HasSessionInput::session_input_mut`] to expose it, and gets
//! `with_session_input_from` for free from the blanket [`WithSessionInput`]
//! impl below.
//!
//! Explicitly NOT `crate::node::NodeExt::with_input_from` — that wraps a
//! node in [`crate::node::WithInput`], whose binding is inert metadata
//! nothing in `process` ever reads (block record `N4`). A node built that
//! way compiles, runs, and silently ignores its binding.

use crate::node::InputBinding;

/// Characters tmux treats as session/window/pane target separators.
/// Sanitized out of every part of the session name before concatenation.
const SANITIZE_CHARS: [char; 2] = [':', '.'];

/// Replace every tmux target-separator character (`:`, `.`) in `part` with
/// `-`. Applied to each input part individually, before concatenation, so
/// the sanitized boundary between `run_id` and `node_identity` can never be
/// reintroduced by a `:`/`.` that happened to appear inside one of them.
fn sanitize(part: &str) -> String {
    part.chars()
        .map(|c| if SANITIZE_CHARS.contains(&c) { '-' } else { c })
        .collect()
}

/// Derive the deterministic, `eng-`-namespaced tmux session name for a
/// terminal node: a pure function of `run_id` + `node_identity` alone, so
/// the same run re-entering the same node identity (a back-edge) always
/// resolves to the same session, and two different node identities under
/// the same run always resolve to different sessions.
///
/// Sanitizes `:` and `.` out of both `run_id` and `node_identity`
/// individually (see [`sanitize`]) before joining them with `_` — a
/// separator that sanitization never produces from either input alone —
/// so the resulting name never contains `:` or `.` and the run/identity
/// boundary cannot collide with a sanitized character from either part.
#[must_use]
pub fn session_name_for(run_id: &str, node_identity: &str) -> String {
    format!("eng-{}_{}", sanitize(run_id), sanitize(node_identity))
}

/// Implemented by a terminal node struct that carries a
/// `session_input: InputBinding` field, exposing a mutable reference to it
/// so the blanket [`WithSessionInput`] impl can supply the builder.
pub trait HasSessionInput: Sized {
    /// Mutable access to this node's `session_input` field.
    fn session_input_mut(&mut self) -> &mut InputBinding;
}

/// Blanket-supplies `with_session_input_from` to any [`HasSessionInput`]
/// implementor — the per-struct field + builder task 1 calls for, without
/// hand-writing the same three lines on every terminal node struct.
pub trait WithSessionInput: HasSessionInput {
    /// Bind this node's session identity to `upstream`'s — i.e. resolve
    /// the session name/lease this node acts on from the
    /// `TerminalSessionNode` identified by `upstream`, rather than from
    /// this node's own default identity.
    #[must_use]
    fn with_session_input_from(mut self, upstream: impl Into<String>) -> Self {
        *self.session_input_mut() = InputBinding::bound(upstream);
        self
    }
}

impl<T: HasSessionInput> WithSessionInput for T {}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal stand-in for a terminal node struct, used only to exercise
    /// the [`HasSessionInput`]/[`WithSessionInput`] blanket builder and
    /// `InputBinding::resolve`'s unbound fallback — the real
    /// `TerminalSessionNode`/`TerminalObserveNode` structs land in tasks
    /// 2/4 and reuse this exact trait pair.
    struct DummyNode {
        session_input: InputBinding,
    }

    impl HasSessionInput for DummyNode {
        fn session_input_mut(&mut self) -> &mut InputBinding {
            &mut self.session_input
        }
    }

    #[test]
    fn session_name_for_is_deterministic() {
        let a = session_name_for("run-1", "TerminalSessionNode");
        let b = session_name_for("run-1", "TerminalSessionNode");
        assert_eq!(a, b);
    }

    #[test]
    fn session_name_for_is_eng_prefixed() {
        let name = session_name_for("run-1", "TerminalSessionNode");
        assert!(name.starts_with("eng-"), "expected eng- prefix, got {name}");
    }

    #[test]
    fn session_name_for_sanitizes_colon_and_dot_from_any_input() {
        let name = session_name_for("run:1.foo", "Node:With.Dots");
        assert!(!name.contains(':'), "unsanitized colon in {name}");
        assert!(!name.contains('.'), "unsanitized dot in {name}");
    }

    #[test]
    fn session_name_for_never_contains_colon_or_dot_for_arbitrary_input() {
        // A broader sweep than the single pinned case above, over inputs
        // that are all-separator, empty, and mixed.
        let cases = [
            ("", ""),
            ("::::", "...."),
            ("a:b:c", "d.e.f"),
            ("run_id-42", "node-identity"),
            (":.:.", ":.:."),
        ];
        for (run_id, node_identity) in cases {
            let name = session_name_for(run_id, node_identity);
            assert!(
                !name.contains(':'),
                "colon leaked for {run_id:?}/{node_identity:?}: {name}"
            );
            assert!(
                !name.contains('.'),
                "dot leaked for {run_id:?}/{node_identity:?}: {name}"
            );
            assert!(name.starts_with("eng-"), "missing eng- prefix: {name}");
        }
    }

    #[test]
    fn distinct_node_identities_under_one_run_id_produce_distinct_names() {
        let a = session_name_for("run-1", "TerminalSessionNode");
        let b = session_name_for("run-1", "TerminalObserveNode");
        assert_ne!(a, b);
    }

    #[test]
    fn distinct_run_ids_under_one_node_identity_produce_distinct_names() {
        let a = session_name_for("run-1", "TerminalSessionNode");
        let b = session_name_for("run-2", "TerminalSessionNode");
        assert_ne!(a, b);
    }

    #[test]
    fn unbound_session_input_resolves_to_caller_supplied_default() {
        let node = DummyNode {
            session_input: InputBinding::default(),
        };
        assert_eq!(
            node.session_input.resolve("DefaultIdentity"),
            "DefaultIdentity"
        );
    }

    #[test]
    fn with_session_input_from_binds_to_upstream_identity() {
        let node = DummyNode {
            session_input: InputBinding::default(),
        }
        .with_session_input_from("UpstreamNode");
        assert_eq!(
            node.session_input.resolve("DefaultIdentity"),
            "UpstreamNode"
        );
    }
}
