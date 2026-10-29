import argparse
import contextlib
import io
import json
import os
import sys
import tempfile
import unittest
from unittest import mock

sys.path.insert(0, os.path.dirname(__file__))
import miri_sweep
from miri_sweep import (
    ALLOWED_STATUS_TRANSITIONS,
    DEFAULT_TARGET_SECONDS,
    DEFAULT_TEST_SECONDS,
    LEAK_PSEUDO_TEST,
    STOPPER_WEIGHT,
    WEIGHTS_PATH,
    ShardPlan,
    assign_units,
    check_target_coverage,
    compare_with_baseline,
    crate_test_targets,
    failure_section,
    isolation_outcome,
    load_target_weights,
    parse_shard,
    planned_tests,
    run_miri,
    scan_outcomes,
    sweep_target,
    work_units,
)


class TestMiriSweepBaselineComparison(unittest.TestCase):
    def setUp(self):
        self.swept_targets = ["lib", "layered_tests"]
        self.models = ["SB", "TB"]

    def test_pass_to_fail_regression(self):
        baseline = {
            "results": {
                "lib::test_foo::SB": "pass",
                "lib::test_bar::SB": "pass",
            }
        }
        results = {
            "lib::test_foo::SB": {"kind": "panic"},
            "lib::test_bar::SB": {"kind": "UB"},
        }
        regs, imps = compare_with_baseline(results, {}, baseline, ["lib"], ["SB"])
        self.assertEqual(len(regs), 2)
        self.assertIn("lib::test_foo::SB: baseline pass -> panic", regs)
        self.assertIn("lib::test_bar::SB: baseline pass -> UB", regs)
        self.assertEqual(imps, [])

    def test_non_pass_to_non_pass_regressions(self):
        # E.g. panic to timeout, or unsupported to panic
        baseline = {
            "results": {
                "lib::test_panic_to_timeout::SB": "panic",
                "lib::test_unsupported_to_panic::SB": "unsupported",
                "lib::test_unsupported_to_timeout::SB": "unsupported",
                "lib::test_panic_to_deadlock::SB": "panic",
                "lib::test_panic_to_ub::SB": "panic",
                "lib::test_ub_to_panic::SB": "UB",
            }
        }
        results = {
            "lib::test_panic_to_timeout::SB": {"kind": "timeout"},
            "lib::test_unsupported_to_panic::SB": {"kind": "panic"},
            "lib::test_unsupported_to_timeout::SB": {"kind": "timeout"},
            "lib::test_panic_to_deadlock::SB": {"kind": "deadlock"},
            "lib::test_panic_to_ub::SB": {"kind": "UB"},
            "lib::test_ub_to_panic::SB": {"kind": "panic"},
        }
        regs, imps = compare_with_baseline(results, {}, baseline, ["lib"], ["SB"])
        self.assertEqual(len(regs), 6)
        self.assertIn("lib::test_panic_to_timeout::SB: baseline panic -> timeout", regs)
        self.assertIn("lib::test_unsupported_to_panic::SB: baseline unsupported -> panic", regs)
        self.assertIn("lib::test_unsupported_to_timeout::SB: baseline unsupported -> timeout", regs)
        self.assertIn("lib::test_panic_to_deadlock::SB: baseline panic -> deadlock", regs)
        self.assertIn("lib::test_panic_to_ub::SB: baseline panic -> UB", regs)
        self.assertIn("lib::test_ub_to_panic::SB: baseline UB -> panic", regs)
        self.assertEqual(imps, [])

    def test_allowed_improvements(self):
        baseline = {
            "results": {
                "lib::test_p::SB": "panic",
                "lib::test_u::SB": "unsupported",
                "lib::test_ub::SB": "UB",
                "lib::test_ign::SB": "ignored",
            }
        }
        results = {
            "lib::test_p::SB": {"kind": "pass"},
            "lib::test_u::SB": {"kind": "pass"},
            "lib::test_ub::SB": {"kind": "pass"},
            "lib::test_ign::SB": {"kind": "pass"},
        }
        regs, imps = compare_with_baseline(results, {}, baseline, ["lib"], ["SB"])
        self.assertEqual(regs, [])
        self.assertEqual(len(imps), 4)
        self.assertIn("lib::test_p::SB: baseline panic -> pass", imps)
        self.assertIn("lib::test_u::SB: baseline unsupported -> pass", imps)
        self.assertIn("lib::test_ub::SB: baseline UB -> pass", imps)
        self.assertIn("lib::test_ign::SB: baseline ignored -> pass", imps)

    def test_unchanged_status_is_neither_regression_nor_improvement(self):
        baseline = {
            "results": {
                "lib::test_pass::SB": "pass",
                "lib::test_panic::SB": "panic",
                "lib::test_unsupported::SB": "unsupported",
                "lib::test_ign::SB": "ignored",
            }
        }
        results = {
            "lib::test_pass::SB": {"kind": "pass"},
            "lib::test_panic::SB": {"kind": "panic"},
            "lib::test_unsupported::SB": {"kind": "unsupported"},
            "lib::test_ign::SB": {"kind": "ignored"},
        }
        regs, imps = compare_with_baseline(results, {}, baseline, ["lib"], ["SB"])
        self.assertEqual(regs, [])
        self.assertEqual(imps, [])

    def test_negative_control_must_produce_ub(self):
        baseline = {
            "negative_controls": ["lib::test_neg_ctrl"],
            "results": {
                "lib::test_neg_ctrl::SB": "UB",
            }
        }
        # If it returns pass or panic instead of UB, it's a regression
        results_pass = {"lib::test_neg_ctrl::SB": {"kind": "pass"}}
        regs, _ = compare_with_baseline(results_pass, {}, baseline, ["lib"], ["SB"])
        self.assertEqual(len(regs), 1)
        self.assertIn("negative control expected UB, got pass", regs[0])

        results_ub = {"lib::test_neg_ctrl::SB": {"kind": "UB"}}
        regs_ub, _ = compare_with_baseline(results_ub, {}, baseline, ["lib"], ["SB"])
        self.assertEqual(regs_ub, [])

    def test_missing_baseline_test_is_regression(self):
        baseline = {
            "results": {
                "lib::test_existing::SB": "pass",
            }
        }
        results = {}
        regs, _ = compare_with_baseline(results, {}, baseline, ["lib"], ["SB"])
        self.assertEqual(len(regs), 1)
        self.assertIn("missing from sweep", regs[0])

    def test_should_panic_outcome_lines_parse_to_bare_name(self):
        text = (
            "test a::plain ... ok\n"
            "test a::boom - should panic ... ok\n"
            "test a::ign - should panic ... ignored\n"
            "test a::bad - should panic ... FAILED\n"
            "test a::skip ... ignored, reason\n"
        )
        self.assertEqual(
            scan_outcomes(text)[0],
            [("a::plain", "ok"), ("a::boom", "ok"), ("a::ign", "ignored"), ("a::bad", "FAILED"), ("a::skip", "ignored")],
        )

    def test_should_panic_failure_section_found_by_bare_name(self):
        text = "---- a::bad - should panic stdout ----\nmsg\n\nfailures:\n"
        self.assertEqual(failure_section(text, "a::bad").strip(), "msg")

    def test_outcome_on_bare_line_after_interleaved_stderr(self):
        text = (
            "test a::slow ... \n"
            "warning: stderr from a::slow\n"
            "  --> src/lib.rs:1:1\n"
            "ok\n"
            "test a::next ... ok\n"
        )
        outcomes, unresolved = scan_outcomes(text)
        self.assertEqual(outcomes, [("a::slow", "ok"), ("a::next", "ok")])
        self.assertEqual(unresolved, [])

    def test_bare_outcome_attributed_to_most_recent_unresolved_start(self):
        text = (
            "test a::first ... \n"
            "test a::second ... ok\n"
            "some stderr\n"
            "FAILED\n"
            "test a::third ... \n"
        )
        outcomes, unresolved = scan_outcomes(text)
        # `second` resolved on its own line, so the bare FAILED goes to `first`, the most recent
        # start still without an outcome.
        self.assertEqual(outcomes, [("a::first", "FAILED"), ("a::second", "ok")])
        self.assertEqual(unresolved, ["a::third"])

    def test_bare_line_without_pending_start_is_ignored(self):
        text = "ok\ntest a::x ... ok\n"
        self.assertEqual(scan_outcomes(text), ([("a::x", "ok")], []))

    def test_started_without_outcome_is_unresolved_stopper(self):
        text = "test a::crash ... \nerror: undefined behavior\n"
        self.assertEqual(scan_outcomes(text), ([], ["a::crash"]))

    def test_isolation_outcome_same_line_and_bare_forms(self):
        same = "test a::t - should panic ... FAILED\n"
        bare = "test a::t ... \nnoise\nignored, reason\n"
        self.assertEqual(isolation_outcome(same, "a::t"), "FAILED")
        self.assertEqual(isolation_outcome(bare, "a::t"), "ignored")
        self.assertIsNone(isolation_outcome("test a::other ... ok\n", "a::t"))
        self.assertIsNone(isolation_outcome("test a::tt ... ok\n", "a::t"))


BASELINE_FILES = {
    "ferrokit-core": "miri_baseline.json",
    "ferrokit-gui": "miri_baseline_ferrokit_gui.json",
    "ferrokit-widgets": "miri_baseline_ferrokit_widgets.json",
}
MODEL_SETS = (["SB"], ["TB"], ["SB", "TB"])


def load_baseline(crate):
    with open(os.path.join(os.path.dirname(__file__), BASELINE_FILES[crate]), "r", encoding="utf-8") as f:
        return json.load(f)


def checked_in_plan(crate, models, count):
    baseline = load_baseline(crate)
    planned = planned_tests(baseline, models)
    return baseline, planned, ShardPlan(baseline["targets"], count, planned, load_target_weights(crate, models))


class TestMiriSweepSharding(unittest.TestCase):
    def test_every_test_runs_in_exactly_one_shard(self):
        # Known tests, and names a new test could have: before, between and after the known ones.
        for crate in BASELINE_FILES:
            for models in MODEL_SETS:
                for count in range(1, 6):
                    baseline, planned, plan = checked_in_plan(crate, models, count)
                    with self.subTest(crate=crate, models=models, count=count):
                        self.assertEqual(
                            {t for i in range(1, count + 1) for t in plan.targets(i)}, set(baseline["targets"])
                        )
                        for target in baseline["targets"]:
                            names = sorted(planned.get(target, ())) + ["", "lib::mm_new_test", "~~last"]
                            ran = [plan.tests(i, target, names) for i in range(1, count + 1)]
                            self.assertEqual(sorted(n for part in ran for n in part), sorted(names), target)
                            for name in names:
                                owners = [i for i in range(1, count + 1) if plan.owns(i)(target, name)]
                                self.assertEqual(len(owners), 1, f"{target}::{name}")
                                self.assertIn(name, plan.tests(owners[0], target, names))

    def test_no_shard_is_empty_and_few_targets_are_split(self):
        for crate in BASELINE_FILES:
            for models in MODEL_SETS:
                for count in range(2, 6):
                    _, _, plan = checked_in_plan(crate, models, count)
                    with self.subTest(crate=crate, models=models, count=count):
                        for i in range(1, count + 1):
                            self.assertTrue(plan.targets(i) and plan.units_of(i), f"shard {i} is empty")
                        split = [t for t in plan.chunks if plan.is_split(t)]
                        self.assertLessEqual(len(split), count - 1, split)

    def test_the_weights_name_exactly_the_baseline_targets_for_both_models(self):
        # A target without a weight silently gets DEFAULT_TARGET_SECONDS and skews the shards (a
        # new test file is easy to forget); a weight without a target is a leftover.
        with open(WEIGHTS_PATH, "r", encoding="utf-8") as f:
            recorded = json.load(f)
        self.assertEqual(set(recorded), set(BASELINE_FILES))
        for crate in BASELINE_FILES:
            with self.subTest(crate=crate):
                targets = set(load_baseline(crate)["targets"])
                self.assertEqual(set(recorded[crate]), targets)
                for target, per_model in recorded[crate].items():
                    self.assertEqual(set(per_model), {"SB", "TB"}, target)

    def test_plan_does_not_depend_on_input_order(self):
        baseline = load_baseline("ferrokit-gui")
        models = ["TB"]
        weights = load_target_weights("ferrokit-gui", models)
        planned = planned_tests(baseline, models)
        shuffled = {t: dict(reversed(list(tests.items()))) for t, tests in reversed(list(planned.items()))}
        first = ShardPlan(baseline["targets"], 2, planned, weights)
        second = ShardPlan(list(reversed(baseline["targets"])), 2, shuffled, weights)
        self.assertEqual(first.chunks, second.chunks)
        self.assertEqual(first.units, second.units)

    def test_shards_are_balanced_by_estimated_seconds(self):
        for crate in BASELINE_FILES:
            for models in MODEL_SETS:
                for count in range(2, 5):
                    _, _, plan = checked_in_plan(crate, models, count)
                    loads = [plan.seconds(i) for i in range(1, count + 1)]
                    heaviest_unit = max(seconds for _, _, seconds in plan.units)
                    # Cutting at unit midpoints keeps every shard within one unit of an even split.
                    self.assertLessEqual(max(loads), sum(loads) / count + heaviest_unit, f"{crate} {models}: {loads}")

    def test_a_target_longer_than_a_shard_is_split_and_the_shards_match(self):
        # ferrokit-gui under Tree Borrows: `lib` alone (about 1500 s on CI) is more than half the work.
        baseline = load_baseline("ferrokit-gui")
        planned = planned_tests(baseline, ["TB"])
        weights = {target: 20 for target in baseline["targets"]}
        weights["lib"] = 1500
        plan = ShardPlan(baseline["targets"], 2, planned, weights)
        self.assertTrue(plan.is_split("lib"))
        self.assertEqual([s for s, _ in plan.chunks["lib"]], [1, 2])
        lib = sorted(planned["lib"])
        first, second = plan.tests(1, "lib", lib), plan.tests(2, "lib", lib)
        self.assertTrue(first and second)
        self.assertEqual(first + second, lib)
        a, b = plan.seconds(1), plan.seconds(2)
        self.assertLess(abs(a - b) / max(a, b), 0.10, (a, b))
        self.assertEqual([t for t in plan.chunks if plan.is_split(t)], ["lib"])

    def test_a_new_test_goes_with_the_chunk_its_name_sorts_into(self):
        planned = {"t": {name: False for name in ("b", "d", "f")}}
        plan = ShardPlan(["t"], 3, planned, {"t": 30})
        self.assertEqual([upper for _, upper in plan.chunks["t"]], ["b", "d", None])
        self.assertEqual(
            [plan.shard_of("t", name) for name in ("a", "b", "c", "d", "e", "f", "zz")], [1, 1, 2, 2, 3, 3, 3]
        )

    def test_unit_seconds_follow_recorded_time_stoppers_and_defaults(self):
        planned = {"t": {"a": True, "b": False, "c": False}, "u": {"x": False}}
        units = work_units(["t", "u", "new"], planned, {"t": 40})
        self.assertEqual(
            units,
            [
                ("new", None, DEFAULT_TARGET_SECONDS),
                ("t", "a", 40 * STOPPER_WEIGHT / (STOPPER_WEIGHT + 2)),
                ("t", "b", 40 / (STOPPER_WEIGHT + 2)),
                ("t", "c", 40 / (STOPPER_WEIGHT + 2)),
                ("u", "x", DEFAULT_TEST_SECONDS),
            ],
        )
        self.assertAlmostEqual(sum(s for tgt, _, s in units if tgt == "t"), 40)

    def test_planned_tests_use_the_requested_models_and_skip_the_leak_pseudo_test(self):
        baseline = {
            "results": {
                "lib::a::SB": "pass",
                "lib::a::TB": "unsupported",
                "lib::b::TB": "pass",
                f"t::{LEAK_PSEUDO_TEST}::SB": "leak",
                "t::c::SB": "UB",
            }
        }
        self.assertEqual(planned_tests(baseline, ["SB"]), {"lib": {"a": False}, "t": {"c": True}})
        self.assertEqual(planned_tests(baseline, ["TB"]), {"lib": {"a": True, "b": False}})
        self.assertEqual(planned_tests(baseline, ["SB", "TB"])["lib"], {"a": True, "b": False})

    def test_assign_units_cuts_evenly_and_never_leaves_a_shard_empty(self):
        self.assertEqual(assign_units([1] * 6, 3), [0, 0, 1, 1, 2, 2])
        self.assertEqual(assign_units([5, 5, 5, 5], 1), [0, 0, 0, 0])
        self.assertEqual(assign_units([1000, 1, 1], 3), [0, 1, 2])
        self.assertEqual(assign_units([1, 1, 1000], 3), [0, 1, 2])
        self.assertEqual(assign_units([0, 0, 0, 0], 2), [0, 0, 1, 1])
        # With fewer units than shards the surplus shards stay empty.
        self.assertEqual(len(set(assign_units([5, 5], 3))), 2)

    def test_assign_units_survives_negative_nan_and_infinite_seconds(self):
        nan, inf = float("nan"), float("inf")
        cases = (
            [-10, -10, -10, 200, 50, -100, -100],
            [nan, 5, 5, nan],
            [inf, 5, -inf, 5],
            [-1, -1, -1],
            [nan, nan],
        )
        for seconds in cases:
            for count in range(1, 5):
                with self.subTest(seconds=seconds, count=count):
                    shards = assign_units(seconds, count)
                    self.assertEqual(len(shards), len(seconds))
                    self.assertTrue(all(0 <= s < count for s in shards), shards)
                    self.assertEqual(shards, sorted(shards))
                    if len(seconds) >= count:
                        self.assertEqual(set(shards), set(range(count)))

    def test_bad_recorded_seconds_never_leave_a_test_without_a_shard(self):
        nan, inf = float("nan"), float("inf")
        baseline = load_baseline("ferrokit-gui")
        planned = planned_tests(baseline, ["TB"])
        targets = baseline["targets"]
        bad = [-50, nan, inf, -inf, 0, 1e9, "slow", True, None]
        weights = {t: bad[i % len(bad)] for i, t in enumerate(targets)}
        weights["lib"] = -3000
        for count in range(1, 5):
            plan = ShardPlan(targets, count, planned, weights)
            for target in targets:
                names = sorted(planned.get(target, ())) + ["", "~~new"]
                owners = [[i for i in range(1, count + 1) if plan.owns(i)(target, n)] for n in names]
                self.assertTrue(all(len(o) == 1 for o in owners), f"{target} count={count}")
            self.assertTrue(all(plan.units_of(i) for i in range(1, count + 1)), count)

    def test_load_target_weights_clamps_negative_and_drops_unusable_entries(self):
        recorded = {
            "c": {
                "neg": {"SB": -5, "TB": 20},
                "nan": {"SB": float("nan"), "TB": 1},
                "inf": {"SB": 1, "TB": float("inf")},
                "text": {"SB": "slow", "TB": 1},
                "ok": {"SB": 3, "TB": 4},
                "onlytb": {"TB": 7},
            }
        }
        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "weights.json")
            with open(path, "w", encoding="utf-8") as f:
                json.dump(recorded, f)  # writes NaN / Infinity, which json.load accepts
            weights = load_target_weights("c", ["SB", "TB"], path)
            self.assertEqual(weights, {"neg": 20, "ok": 7, "onlytb": 7})
            self.assertEqual(load_target_weights("c", ["SB"], path)["neg"], 0)
        units = work_units(["nan", "neg"], {}, weights)
        self.assertEqual(units, [("nan", None, DEFAULT_TARGET_SECONDS), ("neg", None, 20)])

    def test_parse_shard_accepts_valid_and_rejects_invalid(self):
        self.assertEqual(parse_shard("1/2"), (1, 2))
        self.assertEqual(parse_shard("2/2"), (2, 2))
        for bad in ("0/2", "3/2", "1/0", "a/2", "1", "-1/2"):
            with self.assertRaises(argparse.ArgumentTypeError, msg=bad):
                parse_shard(bad)


class TestMiriShardedBaselineComparison(unittest.TestCase):
    def plan(self):
        planned = {"t": {name: False for name in ("a", "b", "c", "d")}}
        return ShardPlan(["t"], 2, planned, {"t": 40})

    def test_a_shard_neither_misses_nor_regresses_the_other_shards_tests(self):
        plan = self.plan()
        baseline = {"results": {f"t::{name}::SB": "pass" for name in "abcd"}}
        for index, mine in ((1, "ab"), (2, "cd")):
            results = {f"t::{name}::SB": {"kind": "pass"} for name in mine}
            regs, imps = compare_with_baseline(results, {}, baseline, ["t"], ["SB"], owns=plan.owns(index))
            self.assertEqual((regs, imps), ([], []), f"shard {index}")

    def test_a_test_the_shard_owns_and_did_not_report_is_still_missing(self):
        plan = self.plan()
        baseline = {"results": {f"t::{name}::SB": "pass" for name in "abcd"}}
        results = {"t::a::SB": {"kind": "pass"}}
        regs, _ = compare_with_baseline(results, {}, baseline, ["t"], ["SB"], owns=plan.owns(1))
        self.assertEqual(regs, ["t::b::SB: baseline test missing from sweep (expected pass)"])

    def test_regressions_in_a_shards_own_tests_are_still_reported(self):
        plan = self.plan()
        baseline = {"results": {f"t::{name}::SB": "pass" for name in "abcd"}}
        results = {"t::c::SB": {"kind": "UB"}, "t::d::SB": {"kind": "pass"}}
        regs, _ = compare_with_baseline(results, {}, baseline, ["t"], ["SB"], owns=plan.owns(2))
        self.assertEqual(regs, ["t::c::SB: baseline pass -> UB"])

    def test_the_leak_pseudo_test_is_judged_only_where_the_whole_target_runs(self):
        leak_key = f"t::{LEAK_PSEUDO_TEST}::SB"
        baseline = {"results": {"t::a::SB": "pass", "t::b::SB": "pass", leak_key: "leak"}}
        results = {"t::a::SB": {"kind": "pass"}, "t::b::SB": {"kind": "pass"}}
        whole = ShardPlan(["t"], 1, planned_tests(baseline, ["SB"]), {"t": 10})
        regs, imps = compare_with_baseline(results, {}, baseline, ["t"], ["SB"], owns=whole.owns(1))
        self.assertEqual((regs, imps), ([], [f"{leak_key}: baseline leak -> no leak"]))
        split = ShardPlan(["t"], 2, planned_tests(baseline, ["SB"]), {"t": 10})
        self.assertTrue(split.is_split("t"))
        for index in (1, 2):
            regs, imps = compare_with_baseline(results, {}, baseline, ["t"], ["SB"], owns=split.owns(index))
            self.assertEqual((regs, imps), ([], []), f"shard {index}")


class TestMiriShardedRuns(unittest.TestCase):
    def test_run_miri_names_exactly_the_chunk_still_to_run(self):
        args = argparse.Namespace(flags="", toolchain="t", crate="c", cross=None, timeout=1)
        with mock.patch("miri_sweep.subprocess.run") as run:
            run.return_value = mock.Mock(stdout="", stderr="", returncode=0)
            run_miri(args, "lib", "SB", {"b"}, only=["c", "a", "b"])
            cmd = run.call_args.args[0]
            self.assertEqual(cmd[cmd.index("--") :], ["--", "--test-threads=1", "--exact", "a", "c"])
            run_miri(args, "lib", "SB", {"b"})
            cmd = run.call_args.args[0]
            self.assertEqual(cmd[cmd.index("--") :], ["--", "--test-threads=1", "--exact", "--skip", "b"])
            with self.assertRaises(ValueError):
                run_miri(args, "lib", "SB", {"a"}, only=["a"])

    def test_sweep_target_asks_for_its_chunk_only_when_partial(self):
        for partial, expected in ((True, ["a", "b"]), (False, None)):
            results, excerpts = {}, {}
            output = ("test a ... ok\ntest b ... ok\ntest result: ok.\n", False)
            with mock.patch("miri_sweep.run_miri", return_value=output) as run:
                sweep_target(None, "t", ["a", "b"], "SB", results, excerpts, partial=partial)
            self.assertEqual(run.call_args.kwargs["only"], expected)
            self.assertEqual(sorted(results), ["t::a::SB", "t::b::SB"])

    @staticmethod
    def fake_run_miri(listed, calls):
        def fake(args, target, model, skip, exact_test=None, extra_miri_flags=None, only=None):
            calls.append((target, exact_test, None if only is None else sorted(only)))
            if exact_test is not None:
                return f"test {exact_test} ... ok\ntest result: ok. 1 passed\n", False
            names = listed[target] if only is None else only
            lines = [f"test {n} ... ok" for n in names if n not in skip]
            return "\n".join(lines + ["test result: ok."]) + "\n", False

        return fake

    def run_main(self, argv, listed, calls):
        with mock.patch.object(sys, "argv", ["miri_sweep.py", *argv]), mock.patch(
            "miri_sweep.list_tests", side_effect=lambda args, target: list(listed[target])
        ), mock.patch("miri_sweep.run_miri", side_effect=self.fake_run_miri(listed, calls)):
            with contextlib.redirect_stdout(io.StringIO()):
                return miri_sweep.main()

    def test_two_shards_together_run_every_test_once_with_no_regressions(self):
        baseline = {
            "crate": "fake-crate",
            "targets": ["lib", "test_x"],
            "results": {
                **{f"lib::{n}::SB": "pass" for n in "abcde"},
                "test_x::x1::SB": "pass",
            },
            "isolation_allowlist": {
                "lib::a": {"reason": "r", "expected": {"SB": "pass"}},
                "lib::e": {"reason": "r", "expected": {"SB": "pass"}},
            },
        }
        # `new_f` is not in the baseline yet.
        listed = {"lib": ["a", "b", "c", "d", "e", "new_f"], "test_x": ["x1"]}
        everything = {f"lib::{n}::SB" for n in listed["lib"]} | {"test_x::x1::SB"}
        outs, all_calls = [], []
        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "baseline.json")
            with open(path, "w", encoding="utf-8") as f:
                json.dump(baseline, f)
            for index in (1, 2):
                out, calls = os.path.join(tmp, f"out{index}.json"), []
                argv = [
                    "--crate", "fake-crate", "--models", "SB", "--baseline", path, "--shard", f"{index}/2",
                    "--run-isolation-allowlist", "--fail-on-regression", "--out", out,
                ]
                self.assertEqual(self.run_main(argv, listed, calls), 0, f"shard {index}")
                with open(out, "r", encoding="utf-8") as f:
                    outs.append(json.load(f))
                all_calls.append(calls)
        first, second = (set(o["results"]) for o in outs)
        self.assertTrue(first and second)
        self.assertFalse(first & second)
        self.assertEqual(first | second, everything)
        for o in outs:
            self.assertEqual((o["regressions"], o["improvements"]), ([], []))
        # The chunks of `lib` were run by name, and together they are all of `lib`.
        lib_runs = [only for calls in all_calls for target, exact, only in calls if target == "lib" and exact is None]
        self.assertEqual(sorted(n for only in lib_runs for n in only), sorted(listed["lib"]))
        # Each allowlisted test is run isolation-disabled by the one shard that owns it.
        iso = [set(o["isolation_results"]) for o in outs]
        self.assertFalse(iso[0] & iso[1])
        self.assertEqual(iso[0] | iso[1], {"lib::a::SB", "lib::e::SB"})

    def test_list_units_shows_each_shards_content_and_the_shards_cover_the_baseline(self):
        path = os.path.join(os.path.dirname(__file__), BASELINE_FILES["ferrokit-gui"])
        baseline = load_baseline("ferrokit-gui")
        lines = []
        for index in (1, 2):
            out = io.StringIO()
            argv = [
                "miri_sweep.py", "--crate", "ferrokit-gui", "--models", "TB", "--baseline", path,
                "--shard", f"{index}/2", "--list-units",
            ]
            # The recorded weights change with every refresh; fix them so that `lib`, by far the
            # heaviest target, is always cut across both shards.
            with mock.patch.object(sys, "argv", argv), contextlib.redirect_stdout(out), mock.patch.object(
                miri_sweep, "load_target_weights", return_value={"lib": 100000}
            ):
                self.assertEqual(miri_sweep.main(), 0)
            lines.append(out.getvalue().splitlines())
            self.assertRegex(lines[-1][-1], rf"^# shard {index}/2: \d+ units, ~\d+ s \(estimate per shard: \d+, \d+\)$")
            self.assertTrue(any(line.startswith("# lib: chunk") for line in lines[-1]))
        units = [[line.split("\t")[0] for line in part if not line.startswith("#")] for part in lines]
        planned = planned_tests(baseline, ["TB"])
        expected = [f"{target}::{test}" for target, tests in planned.items() for test in tests]
        # A target the baseline lists no test for is one `target::*` unit.
        expected += [f"{t}::*" for t in baseline["targets"] if t not in planned]
        self.assertEqual(sorted(units[0] + units[1]), sorted(expected))


class TestMiriLeakBaseline(unittest.TestCase):
    LEAK_KEY = "t::(leaks at process exit)::SB"

    def test_recorded_leak_is_not_a_regression_and_its_fix_is_an_improvement(self):
        baseline = {"results": {"t::a::SB": "pass", self.LEAK_KEY: "leak"}}
        still = {"t::a::SB": {"kind": "pass"}, self.LEAK_KEY: {"kind": "leak"}}
        self.assertEqual(compare_with_baseline(still, {}, baseline, ["t"], ["SB"]), ([], []))
        fixed = {"t::a::SB": {"kind": "pass"}}
        regs, imps = compare_with_baseline(fixed, {}, baseline, ["t"], ["SB"])
        self.assertEqual(regs, [])
        self.assertEqual(imps, [f"{self.LEAK_KEY}: baseline leak -> no leak"])

    def test_new_leak_is_a_regression(self):
        baseline = {"results": {"t::a::SB": "pass"}}
        leaking = {"t::a::SB": {"kind": "pass"}, self.LEAK_KEY: {"kind": "leak"}}
        regs, _ = compare_with_baseline(leaking, {}, baseline, ["t"], ["SB"])
        self.assertEqual(regs, [f"{self.LEAK_KEY}: new test not in baseline failed with leak"])

    def test_a_missing_ordinary_test_is_still_a_regression(self):
        baseline = {"results": {"t::a::SB": "leak"}}
        regs, _ = compare_with_baseline({}, {}, baseline, ["t"], ["SB"])
        self.assertEqual(len(regs), 1)


class TestMiriBaselineFiles(unittest.TestCase):
    @staticmethod
    def baseline_paths():
        here = os.path.dirname(__file__)
        return sorted(
            os.path.join(here, name)
            for name in os.listdir(here)
            if name.startswith("miri_baseline") and name.endswith(".json")
        )

    def test_every_baseline_is_self_consistent(self):
        paths = self.baseline_paths()
        self.assertGreaterEqual(len(paths), 3, paths)
        crates = set()
        for path in paths:
            with open(path, "r", encoding="utf-8") as f:
                baseline = json.load(f)
            crates.add(baseline["crate"])
            targets = baseline["targets"]
            self.assertEqual(len(targets), len(set(targets)), path)
            for key in baseline["results"]:
                self.assertIn(key.split("::", 1)[0], targets, f"{path}: {key}")
                self.assertIn(key.rsplit("::", 1)[1], ("SB", "TB"), f"{path}: {key}")
        self.assertEqual(len(crates), len(paths), "each baseline file must cover a distinct crate")


class TestMiriTargetCoverage(unittest.TestCase):
    def test_complete_baseline_has_no_problems(self):
        baseline = {"targets": ["lib", "a"], "excluded": {"b": "cfg(windows) only"}}
        self.assertEqual(check_target_coverage(baseline, {"a", "b"}), [])

    def test_missing_excluded_key_is_empty_map(self):
        self.assertEqual(check_target_coverage({"targets": ["a"]}, {"a"}), [])

    def test_unlisted_test_file_is_a_problem(self):
        problems = check_target_coverage({"targets": ["a"]}, {"a", "b"})
        self.assertEqual(len(problems), 1)
        self.assertIn("tests/b.rs", problems[0])

    def test_target_in_both_maps_is_a_problem(self):
        problems = check_target_coverage({"targets": ["a"], "excluded": {"a": "why"}}, {"a"})
        self.assertEqual(len(problems), 1)
        self.assertIn("both", problems[0])

    def test_excluded_reason_must_be_non_empty_string(self):
        for reason in ("", "   ", None, 3):
            with self.subTest(reason=reason):
                problems = check_target_coverage({"targets": [], "excluded": {"a": reason}}, {"a"})
                self.assertEqual(len(problems), 1)
                self.assertIn("reason", problems[0])

    def test_stale_entries_are_problems(self):
        baseline = {"targets": ["lib", "gone"], "excluded": {"old": "removed"}}
        problems = check_target_coverage(baseline, set())
        self.assertEqual(len(problems), 2)
        self.assertTrue(any("gone" in p for p in problems))
        self.assertTrue(any("old" in p for p in problems))

    def test_lib_needs_no_test_file(self):
        self.assertEqual(check_target_coverage({"targets": ["lib"]}, set()), [])

    def test_excluded_must_be_a_mapping(self):
        problems = check_target_coverage({"targets": [], "excluded": ["a"]}, {"a"})
        self.assertEqual(len(problems), 1)

    def test_crate_test_targets_lists_top_level_rust_files_only(self):
        with tempfile.TemporaryDirectory() as root:
            tests = os.path.join(root, "crates", "demo", "tests")
            os.makedirs(os.path.join(tests, "support"))
            for name in ("a.rs", "b.txt", os.path.join("support", "c.rs")):
                open(os.path.join(tests, name), "w").close()
            self.assertEqual(crate_test_targets("demo", root), {"a"})

    def test_checked_in_baselines_account_for_every_test_file(self):
        here = os.path.dirname(os.path.abspath(__file__))
        for name in sorted(os.listdir(here)):
            if not (name.startswith("miri_baseline") and name.endswith(".json")):
                continue
            with open(os.path.join(here, name), "r", encoding="utf-8") as f:
                baseline = json.load(f)
            with self.subTest(baseline=name):
                self.assertEqual(check_target_coverage(baseline, crate_test_targets(baseline["crate"])), [])


class TestMiriTimeoutClassification(unittest.TestCase):
    @staticmethod
    def sweep(outputs, names):
        results, excerpts = {}, {}
        with mock.patch("miri_sweep.run_miri", side_effect=outputs):
            sweep_target(None, "t", names, "SB", results, excerpts)
        return {key: entry["kind"] for key, entry in results.items()}

    def test_hang_before_any_test_starts_times_out_every_test(self):
        kinds = self.sweep([("", True)], ["a", "b"])
        self.assertEqual(kinds, {"t::a::SB": "timeout", "t::b::SB": "timeout"})

    def test_hang_inside_a_test_times_out_that_test_and_the_sweep_continues(self):
        kinds = self.sweep([("test a ... ", True), ("test b ... ok\ntest result: ok. 1 passed\n", False)], ["a", "b"])
        self.assertEqual(kinds, {"t::a::SB": "timeout", "t::b::SB": "pass"})



if __name__ == "__main__":
    unittest.main()


class TestMiriFloatDeterminism(unittest.TestCase):
    def test_every_run_uses_deterministic_floats(self):
        args = argparse.Namespace(flags="", toolchain="t", crate="c", cross=None, timeout=1)
        for model, extra in (("SB", None), ("TB", None), ("SB", ["-Zmiri-disable-isolation"])):
            with self.subTest(model=model, extra=extra), mock.patch("miri_sweep.subprocess.run") as run:
                run.return_value = mock.Mock(stdout="", stderr="", returncode=0)
                run_miri(args, "lib", model, set(), extra_miri_flags=extra)
                flags = run.call_args.kwargs["env"]["MIRIFLAGS"].split()
                self.assertIn("-Zmiri-deterministic-floats", flags)

