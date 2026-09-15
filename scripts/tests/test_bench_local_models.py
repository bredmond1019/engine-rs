#!/usr/bin/env python3
"""Fixture suite for scripts/bench_local_models.py.

The properties an unattended overnight sweep depends on, each pinned here:

- build_harness_from_tasks reads tasks.json (positive control: output changes
  when input does) -- a hand-duplicated harness once failed every dispatch.
- A job that blows up returns a structured record instead of propagating.
- Resume skips recorded jobs but retries infrastructure failures.
- Jobs run rep -> tier -> model -> backend, so a cut-short sweep stays balanced.
- A timed-out run is aborted, and an abort that never lands stops the sweep
  WITHOUT cleaning the block out from under the still-live run.
- classify() separates model failures from engine anomalies.
- child_sdlc_flow_policy nests under data.policy (a top-level copy is ignored
  and falls back to paid Claude defaults).
- The generated leaderboard carries OKF frontmatter, since it lives in the corpus.
"""

from __future__ import annotations

import json
import sys
import tempfile
import unittest
from datetime import datetime
from pathlib import Path
from unittest import mock

REPO_ROOT = Path(__file__).resolve().parent.parent.parent
sys.path.insert(0, str(REPO_ROOT / "scripts"))

import bench_local_models as blm  # noqa: E402


def make_cfg(run_dir: Path, **overrides) -> blm.SweepConfig:
    values = dict(
        bastion_addr="http://localhost:0",
        api_key="fake-key",
        endpoint="http://localhost:11434",
        work_id="bench-test-fixture",
        review_mode="end_only",
        call_timeout_seconds=None,
        poll_interval=0.01,
        timeout_minutes=0.01,
        abort_grace_minutes=0.01,
        run_dir=run_dir,
    )
    values.update(overrides)
    return blm.SweepConfig(**values)


class TestSlugify(unittest.TestCase):
    def test_replaces_unsafe_characters(self) -> None:
        self.assertEqual(blm.slugify("qwen2.5-coder:7b"), "qwen2.5-coder-7b")
        self.assertEqual(blm.slugify("plain-name"), "plain-name")


class TestBuildHarnessFromTasks(unittest.TestCase):
    def _tasks(self, filename: str, marker: str) -> list[dict]:
        return [{"task_id": 1, "title": f"Write {filename}", "validation_commands": [f"test -f {filename}", f"grep -q {marker} {filename}"]}]

    def test_generates_one_gating_check_per_task_with_validation_commands(self) -> None:
        checks = blm.build_harness_from_tasks(self._tasks("LOCAL_ORCH_1.md", "LOCAL-ORCH-1"))["validation"]["checks"]
        self.assertEqual(len(checks), 1)
        self.assertEqual(checks[0]["name"], "task-1")
        self.assertIn("LOCAL_ORCH_1.md", checks[0]["command"])
        self.assertTrue(checks[0]["gates"])

    def test_task_with_no_validation_commands_produces_no_check(self) -> None:
        harness = blm.build_harness_from_tasks([{"task_id": 1, "title": "no-op", "validation_commands": []}])
        self.assertEqual(harness["validation"]["checks"], [])

    def test_harness_changes_when_tasks_change(self) -> None:
        a = blm.build_harness_from_tasks(self._tasks("FOO.md", "MARKER-FOO"))["validation"]["checks"]
        b = blm.build_harness_from_tasks(self._tasks("BAR.md", "MARKER-BAR"))["validation"]["checks"]
        self.assertNotEqual(a, b)
        self.assertNotIn("FOO.md", b[0]["command"])

    def test_every_real_tier_parses_and_declares_files(self) -> None:
        tier_dirs = sorted(p for p in blm.TIERS_DIR.iterdir() if (p / "tasks.json").is_file())
        self.assertGreaterEqual(len(tier_dirs), 5, [p.name for p in tier_dirs])
        for tier_dir in tier_dirs:
            tasks = json.loads((tier_dir / "tasks.json").read_text())
            for task in tasks:
                self.assertTrue(task.get("files"), f"{tier_dir.name} task {task['task_id']} declares no files[]")
                self.assertTrue(task.get("validation_commands"), f"{tier_dir.name} task {task['task_id']} has no checks")


class TestMissingFixturePaths(unittest.TestCase):
    def test_flags_repo_paths_absent_from_origin_main_and_missing_vault_paths(self) -> None:
        tasks = [{"validation_commands": ["python3 scripts/definitely_not_a_real_file_xyz.py", "bash planning/nope/never.sh"], "description": ""}]
        missing = blm.missing_fixture_paths(tasks)
        self.assertEqual(len(missing), 2, missing)

    def test_positive_control_existing_paths_are_not_flagged(self) -> None:
        tasks = [{"validation_commands": ["python3 scripts/dev-tooling/bench_verify_medium.py"], "description": "edit planning/local-model-bench/index.md"}]
        self.assertEqual(blm.missing_fixture_paths(tasks), [])


class TestCommitSubjectCollisions(unittest.TestCase):
    def test_flags_a_title_already_committed_on_origin_main(self) -> None:
        """Positive control: `feat(sdlc): 2 — Write a greeting function` is on
        origin/main (the merged bench branch), so that title must be refused."""
        tasks = {"old-easy": [{"task_id": 2, "title": "Write a greeting function"}]}
        self.assertEqual(len(blm.commit_subject_collisions(tasks)), 1)

    def test_every_real_tier_title_is_unique_against_origin_main(self) -> None:
        tiers = {
            p.name: json.loads((p / "tasks.json").read_text())
            for p in blm.TIERS_DIR.iterdir()
            if (p / "tasks.json").is_file()
        }
        self.assertEqual(blm.commit_subject_collisions(tiers), [])


class TestPlanAndResume(unittest.TestCase):
    def test_job_order_is_rep_then_tier_then_model_then_backend(self) -> None:
        jobs = blm.plan_jobs(["easy", "hard"], ["m1", "m2"], ["aider", "pi"], 2)
        self.assertEqual(len(jobs), 16)
        self.assertEqual(jobs[0], blm.JobSpec("easy", "m1", "aider", 1))
        self.assertEqual(jobs[1], blm.JobSpec("easy", "m1", "pi", 1))
        self.assertEqual(jobs[2], blm.JobSpec("easy", "m2", "aider", 1))
        self.assertEqual(jobs[4].tier, "hard")
        self.assertTrue(all(j.rep == 1 for j in jobs[:8]))

    def test_resume_skips_recorded_jobs_but_retries_infra_failures(self) -> None:
        with tempfile.TemporaryDirectory() as d:
            run_dir = Path(d)
            done, infra, garbage, absent = (blm.JobSpec("easy", f"m{i}", "aider", 1) for i in range(4))
            for spec, outcome in ((done, "task_failed"), (infra, "dispatch_error")):
                path = spec.record_path(run_dir)
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(json.dumps({"outcome": outcome}))
            garbage.record_path(run_dir).write_text("{not json")
            self.assertIsNotNone(blm.completed_record(done.record_path(run_dir)))
            self.assertIsNone(blm.completed_record(infra.record_path(run_dir)))
            self.assertIsNone(blm.completed_record(garbage.record_path(run_dir)))
            self.assertIsNone(blm.completed_record(absent.record_path(run_dir)))

    def test_deadline_in_the_past_rolls_to_tomorrow(self) -> None:
        now = datetime(2026, 9, 14, 23, 0)
        self.assertEqual(blm.parse_deadline("07:30", now), datetime(2026, 9, 15, 7, 30))
        self.assertEqual(blm.parse_deadline("23:30", now), datetime(2026, 9, 14, 23, 30))
        self.assertIsNone(blm.parse_deadline(None, now))


class TestEventBody(unittest.TestCase):
    def test_direct_sdlc_flow_event_carries_policy_and_no_block_id(self) -> None:
        """No block_id anywhere: through ORCHESTRATION a passing job closed its
        block and ran a fleet-wide emit-state."""
        with tempfile.TemporaryDirectory() as d:
            cfg = make_cfg(Path(d), call_timeout_seconds=900)
            body = blm.build_event_body(blm.JobSpec("hard", "qwen3:8b", "pi", 1), cfg)
        self.assertEqual(body["workflow_type"], "SDLC_FLOW")
        data = body["data"]
        self.assertNotIn("block_id", json.dumps(body))
        self.assertEqual(data["spec_slug"], "bench-test-fixture")
        self.assertTrue(data["use_worktree"])
        self.assertFalse(data["auto_pr"])
        policy = data["policy"]
        self.assertEqual(policy["agent_backend"], "pi")
        self.assertEqual(policy["local"]["model"], "qwen3:8b")
        self.assertEqual(policy["review_mode"], "end_only")
        self.assertEqual(policy["test_dispatch"], "inline")
        self.assertEqual(policy["timeouts"]["implement"], 900)

    def test_real_registered_block_id_is_refused_as_spec_slug(self) -> None:
        self.assertTrue(blm.block_id_registered("bench-easy"))
        self.assertFalse(blm.block_id_registered("local-model-bench-run"))

    def test_local_model_uses_the_context_variant_when_preflight_resolved_one(self) -> None:
        with tempfile.TemporaryDirectory() as d:
            cfg = make_cfg(Path(d))
            cfg.model_info = {"qwen3:8b": {"ollama_model": "qwen3:8b-ctx16384"}}
            with_variant = blm.build_event_body(blm.JobSpec("easy", "qwen3:8b", "pi", 1), cfg)
            without = blm.build_event_body(blm.JobSpec("easy", "llama3.2:3b", "pi", 1), cfg)
        self.assertEqual(with_variant["data"]["policy"]["local"]["model"], "qwen3:8b-ctx16384")
        self.assertEqual(without["data"]["policy"]["local"]["model"], "llama3.2:3b")

    def test_all_models_skips_existing_context_variants(self) -> None:
        tags = {"models": [
            {"name": "qwen2.5:7b-instruct-ctx16384", "size": 5},
            {"name": "qwen2.5:7b-instruct", "size": 4},
            {"name": "llama3.2:3b", "size": 2},
        ]}
        with mock.patch.object(blm, "http_get_json", return_value=tags):
            models = blm.resolve_models("all", "http://x")
        self.assertEqual(models, ["llama3.2:3b", "qwen2.5:7b-instruct"])

    def test_resolve_required_capability_auto_requires_tools_for_coding_agent_backends(self) -> None:
        self.assertEqual(blm.resolve_required_capability("auto", ["aider", "pi"]), "tools")
        self.assertEqual(blm.resolve_required_capability("auto", ["pi"]), "tools")

    def test_resolve_required_capability_auto_falls_back_to_completion_for_non_coding_backends(self) -> None:
        self.assertEqual(blm.resolve_required_capability("auto", []), "completion")
        self.assertEqual(blm.resolve_required_capability("auto", ["some-future-backend"]), "completion")

    def test_resolve_required_capability_explicit_choice_overrides_auto(self) -> None:
        self.assertEqual(blm.resolve_required_capability("completion", ["aider", "pi"]), "completion")
        self.assertEqual(blm.resolve_required_capability("tools", []), "tools")

    def test_ctx_variant_names(self) -> None:
        self.assertEqual(blm.ctx_variant_name("qwen2.5:7b-instruct", 16384), "qwen2.5:7b-instruct-ctx16384")
        self.assertEqual(blm.ctx_variant_name("gpt-oss", 8192), "gpt-oss:latest-ctx8192")

    def test_no_timeouts_key_when_unset(self) -> None:
        with tempfile.TemporaryDirectory() as d:
            body = blm.build_event_body(blm.JobSpec("easy", "m", "aider", 1), make_cfg(Path(d)))
        self.assertNotIn("timeouts", body["data"]["policy"])


class TestPreflightCapabilityFilter(unittest.TestCase):
    """The bug this closes: --models all dispatched tools-incapable models
    (phi3.5:3.8b, codestral:22b) through aider/pi, which always send tool
    definitions -- Ollama hard-rejects with HTTP 400 before generation starts.
    Capability must come from a live query, never a hardcoded name list."""

    def _run_preflight(self, *, backends: list[str], require_capability: str, tags: dict, shows: dict):
        def fake_http_status(url, *_a, **_k):
            if url.endswith("/health"):
                return 200
            if url.endswith("/api/tags"):
                return 200
            return 200

        def fake_http_get_json(url, *_a, **_k):
            if url.endswith("/api/tags"):
                return tags
            return {}

        def fake_http_post_json(url, _api_key, payload, *_a, **_k):
            if url.endswith("/api/show"):
                return shows[payload["model"]]
            return {}

        with mock.patch.multiple(
            blm,
            http_status=fake_http_status,
            http_get_json=fake_http_get_json,
            http_post_json=fake_http_post_json,
            commit_subject_collisions=mock.MagicMock(return_value=[]),
            tasks_already_satisfied=mock.MagicMock(return_value=[]),
            shutil=mock.MagicMock(which=mock.MagicMock(return_value="/usr/bin/fake")),
        ):
            return blm.preflight(
                tiers=[], models_arg="all", backends=backends,
                endpoint="http://x", bastion_addr="http://y", api_key="k",
                num_ctx=0, create_variants=False, require_capability=require_capability,
            )

    def test_tools_incapable_model_excluded_when_backend_requires_tools(self) -> None:
        tags = {"models": [{"name": "phi3.5:3.8b", "size": 1}, {"name": "qwen3:8b", "size": 2}]}
        shows = {
            "phi3.5:3.8b": {"capabilities": ["completion"], "details": {}},
            "qwen3:8b": {"capabilities": ["completion", "tools"], "details": {}},
        }
        problems, warnings, excluded, runnable, info, all_caps = self._run_preflight(
            backends=["aider", "pi"], require_capability="tools", tags=tags, shows=shows,
        )
        self.assertEqual(problems, [])
        self.assertEqual(runnable, ["qwen3:8b"])
        excluded_models = {e["model"] for e in excluded}
        self.assertIn("phi3.5:3.8b", excluded_models)
        self.assertIn("tools", next(e["reason"] for e in excluded if e["model"] == "phi3.5:3.8b"))
        self.assertEqual(len(all_caps), 2, "capability record kept for every resolved model, runnable or not")

    def test_same_model_is_runnable_when_only_completion_is_required(self) -> None:
        tags = {"models": [{"name": "phi3.5:3.8b", "size": 1}]}
        shows = {"phi3.5:3.8b": {"capabilities": ["completion"], "details": {}}}
        problems, warnings, excluded, runnable, info, all_caps = self._run_preflight(
            backends=[], require_capability="completion", tags=tags, shows=shows,
        )
        self.assertEqual(excluded, [])
        self.assertEqual(runnable, ["phi3.5:3.8b"])

    def test_embedding_only_model_excluded_regardless_of_capability_floor(self) -> None:
        tags = {"models": [{"name": "bge-m3:latest", "size": 1}]}
        shows = {"bge-m3:latest": {"capabilities": ["embedding"], "details": {}}}
        _problems, _warnings, excluded, runnable, _info, _all_caps = self._run_preflight(
            backends=[], require_capability="completion", tags=tags, shows=shows,
        )
        self.assertEqual(runnable, [])
        self.assertEqual(excluded[0]["reason"], "no completion capability ['embedding']")


class TestRenderCapabilityReport(unittest.TestCase):
    def test_separates_tools_capable_from_completion_only_and_lists_exclusions(self) -> None:
        all_caps = [
            {"model": "qwen3:8b", "capabilities": ["completion", "tools"], "parameter_size": "8B", "quantization_level": "Q4", "family": "qwen3"},
            {"model": "phi3.5:3.8b", "capabilities": ["completion"], "parameter_size": "3.8B", "quantization_level": "Q4", "family": "phi3"},
        ]
        excluded = [{"model": "phi3.5:3.8b", "capabilities": ["completion"], "reason": "no tools capability"}]
        with tempfile.TemporaryDirectory() as d:
            with mock.patch.object(blm, "BENCH_DIR", Path(d)):
                text = blm.render_capability_report(all_caps, excluded, "tools", ["aider", "pi"])
                self.assertTrue((Path(d) / "model-capabilities.json").is_file())
        self.assertIn("qwen3:8b", text.split("## Completion-only")[0])
        self.assertIn("phi3.5:3.8b", text.split("## Completion-only")[1].split("## Excluded")[0])
        self.assertIn("no tools capability", text)
        self.assertIn("SummarizeNode", text)


class TestClassify(unittest.TestCase):
    def _c(self, engine_ok: bool, checks: list[bool], changed: list[str], declared=frozenset({"a.py"})) -> str:
        return blm.classify(
            engine_tasks=[{"task_id": i + 1, "passed": engine_ok} for i in range(len(checks) or 1)],
            final_checks=[{"task_id": i + 1, "passed": p} for i, p in enumerate(checks)],
            changed=changed,
            declared=set(declared),
        )

    def test_correct_first_task_exhausted_while_later_task_never_ran_is_an_engine_anomaly(self) -> None:
        """The 2026-09-14 smoke: task 1's file was right, the engine burned all
        attempts on it, task 2 never ran -- not a model failure."""
        category = blm.classify(
            engine_tasks=[
                {"task_id": 1, "passed": False, "attempts_exhausted": True},
                {"task_id": 2, "passed": False, "attempts_exhausted": False},
            ],
            final_checks=[{"task_id": 1, "passed": True}, {"task_id": 2, "passed": False}],
            changed=["LOCAL_ORCH_1.md"],
            declared={"LOCAL_ORCH_1.md", "bench_greet.py"},
        )
        self.assertEqual(category, "engine_rejected_correct_work")
        control = blm.classify(
            engine_tasks=[
                {"task_id": 1, "passed": True, "attempts_exhausted": False},
                {"task_id": 2, "passed": False, "attempts_exhausted": True},
            ],
            final_checks=[{"task_id": 1, "passed": True}, {"task_id": 2, "passed": False}],
            changed=["LOCAL_ORCH_1.md", "bench_greet.py"],
            declared={"LOCAL_ORCH_1.md", "bench_greet.py"},
        )
        self.assertEqual(control, "check_failed")

    def test_categories(self) -> None:
        self.assertEqual(self._c(True, [True], ["a.py"]), "passed")
        self.assertEqual(self._c(False, [True], ["a.py"]), "engine_rejected_correct_work")
        self.assertEqual(self._c(True, [False], ["a.py"]), "engine_accepted_failing_work")
        self.assertEqual(self._c(False, [False], []), "no_change")
        self.assertEqual(self._c(False, [False], ["path/to/a.py"]), "wrong_path")
        self.assertEqual(self._c(False, [True, False], ["a.py"]), "check_failed")

    def test_no_final_checks_is_never_passed(self) -> None:
        self.assertNotEqual(self._c(True, [], ["a.py"]), "passed")


class TestHarvestState(unittest.TestCase):
    def test_derives_pass_fail_from_dict_keyed_tasks(self) -> None:
        with tempfile.TemporaryDirectory() as d:
            state_file = Path(d) / "sdlc-flow-state.json"
            state_file.write_text(json.dumps({
                "outcomes": {"backend_used": {"ImplementTaskNode": "aider"}, "total_cost_usd": 0.0, "total_attempts": 4},
                "bail_reason": "Max attempts (3) reached",
                "engine_build_sha": "abc123",
                "tasks": {
                    "2": {"task_id": 2, "title": "t2", "status": "done", "attempt_count": 0, "max_attempts": 3},
                    "1": {"task_id": 1, "title": "t1", "status": "pending", "attempt_count": 3, "max_attempts": 3},
                },
            }))
            record = blm.JobRecord(model="m", backend="aider")
            blm.harvest_state(record, state_file)
        self.assertEqual([t["task_id"] for t in record.tasks], [1, 2])
        self.assertFalse(record.tasks[0]["passed"])
        self.assertTrue(record.tasks[0]["attempts_exhausted"])
        self.assertTrue(record.tasks[1]["passed"])
        self.assertEqual(record.engine_build_sha, "abc123")

    def test_missing_state_file_is_handled_gracefully(self) -> None:
        record = blm.JobRecord(model="m", backend="aider")
        blm.harvest_state(record, Path("/nonexistent/state.json"))
        self.assertEqual(record.tasks, [])


class TestPollRun(unittest.TestCase):
    def test_returns_timeout_status_rather_than_hanging(self) -> None:
        with mock.patch.object(blm, "http_get_json", return_value={"status": "running"}), \
             mock.patch.object(blm.time, "sleep", return_value=None):
            status, _ = blm.poll_run("http://x", "key", "run1", poll_interval=0.01, timeout_minutes=0)
        self.assertEqual(status, "timeout")

    def test_transient_poll_error_does_not_abort_polling(self) -> None:
        calls = {"n": 0}

        def flaky_get(*_args, **_kwargs):
            calls["n"] += 1
            if calls["n"] < 3:
                raise blm.urllib.error.URLError("connection refused")
            return {"status": "succeeded"}

        with mock.patch.object(blm, "http_get_json", side_effect=flaky_get), \
             mock.patch.object(blm.time, "sleep", return_value=None):
            status, _ = blm.poll_run("http://x", "key", "run1", poll_interval=0.01, timeout_minutes=5)
        self.assertEqual(status, "succeeded")


class TestRunOneJob(unittest.TestCase):
    def _run(self, **patches) -> tuple[blm.JobRecord, dict]:
        mocks: dict = {}
        with tempfile.TemporaryDirectory() as d:
            run_dir = Path(d)
            work_parent = Path(d) / "planning"
            defaults = {
                "clean_block": mock.MagicMock(return_value=None),
                "capture_evidence": mock.MagicMock(return_value=run_dir / "artifacts" / "x"),
                "run_final_checks": mock.MagicMock(return_value=[{"task_id": 1, "passed": True, "output": ""}]),
                "changed_paths": mock.MagicMock(return_value=["LOCAL_ORCH_1.md"]),
                "http_get_json": mock.MagicMock(return_value={}),
            }
            defaults.update(patches)
            with mock.patch.multiple(blm, **defaults), mock.patch.object(blm, "REPO_DIR", work_parent.parent):
                record = blm.run_one_job(blm.JobSpec("easy", "totally-fake-model", "aider", 1), make_cfg(run_dir))
            mocks.update(defaults)
        return record, mocks

    def test_dispatch_exception_yields_a_structured_record_not_a_raise(self) -> None:
        record, mocks = self._run(http_post_json=mock.MagicMock(side_effect=RuntimeError("simulated network failure")))
        self.assertEqual(record.outcome, "exception")
        self.assertIn("simulated network failure", record.error or "")
        self.assertIsNotNone(record.error_traceback)
        self.assertNotEqual(record.ended_at, "")
        self.assertGreaterEqual(mocks["clean_block"].call_count, 2, "block must be cleaned after a failed job too")

    def test_missing_run_id_is_a_recorded_dispatch_error(self) -> None:
        record, _ = self._run(http_post_json=mock.MagicMock(return_value={"no_run_id_here": True}))
        self.assertEqual(record.outcome, "dispatch_error")
        self.assertIn("run_id", record.error or "")

    def test_timeout_aborts_the_run_and_records_timeout(self) -> None:
        post = mock.MagicMock(return_value={"run_id": "r-1"})
        record, _ = self._run(
            http_post_json=post,
            poll_run=mock.MagicMock(side_effect=[("timeout", 1.0), ("cancelled", 0.1)]),
        )
        self.assertTrue(any(c.args[0].endswith("/events/r-1/abort") for c in post.call_args_list), post.call_args_list)
        self.assertEqual(record.outcome, "timeout")
        self.assertTrue(record.timed_out)
        self.assertFalse(record.fatal)

    def test_unconfirmed_abort_is_fatal_and_leaves_the_block_alone(self) -> None:
        record, mocks = self._run(
            http_post_json=mock.MagicMock(return_value={"run_id": "r-2"}),
            poll_run=mock.MagicMock(side_effect=[("timeout", 1.0), ("timeout", 1.0)]),
        )
        self.assertTrue(record.fatal)
        self.assertEqual(record.failure_category, "abort_unconfirmed")
        self.assertEqual(mocks["clean_block"].call_count, 1, "must not clean the block while its run may still be live")

    def test_engine_rejection_of_correct_work_is_flagged(self) -> None:
        record, _ = self._run(
            http_post_json=mock.MagicMock(return_value={"run_id": "r-3"}),
            poll_run=mock.MagicMock(return_value=("failed", 1.0)),
        )
        self.assertEqual(record.run_status, "failed")
        self.assertEqual(record.failure_category, "engine_rejected_correct_work")
        self.assertEqual(record.outcome, "task_failed")


class TestRenderReports(unittest.TestCase):
    def _write(self, run_dir: Path, **record) -> None:
        spec = blm.JobSpec(record["tier"], record["model"], record["backend"], record.get("rep", 1))
        path = spec.record_path(run_dir)
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps({"rep": 1, **record}))

    def test_leaderboard_has_frontmatter_scores_and_excludes_artifacts_and_infra(self) -> None:
        with tempfile.TemporaryDirectory() as d:
            run_dir = Path(d)
            self._write(run_dir, tier="easy", model="m-a", backend="aider", outcome="passed", failure_category="passed", wall_clock_seconds=30)
            self._write(run_dir, tier="hard", model="m-a", backend="aider", outcome="task_failed", failure_category="wrong_path")
            self._write(run_dir, tier="easy", model="m-b", backend="pi", outcome="dispatch_error", failure_category="infra")
            self._write(run_dir, tier="easy", model="m-c", backend="pi", outcome="task_failed", failure_category="engine_rejected_correct_work", artifacts_dir="/tmp/ev")
            (run_dir / "artifacts" / "job" ).mkdir(parents=True)
            (run_dir / "artifacts" / "job" / "run-event.json").write_text(json.dumps({"model": "x", "tier": "y"}))

            text = blm.render_reports(run_dir, "2026-09-14")
            summary = json.loads((run_dir / "summary.json").read_text())

        self.assertTrue(text.startswith("---\ntype: Reference\n"))
        self.assertIn("doc_id: local-model-bench-results-2026-09-14", text)
        self.assertEqual(len(summary), 4)
        self.assertIn("| m-a | - | aider | 1/1 | 0/1 | 1/2 |", text)
        self.assertIn("| m-b | - | pi | - | - | 0/0 |", text)
        self.assertIn("engine_rejected_correct_work", text.split("## Engine anomalies")[1].split("## Jobs")[0])


if __name__ == "__main__":
    unittest.main()
