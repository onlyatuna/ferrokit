import argparse
import json
import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(__file__))
from miri_sweep import (
    compare_with_baseline,
    parse_shard,
    shard_targets,
    ALLOWED_STATUS_TRANSITIONS,
    failure_section,
    isolation_outcome,
    scan_outcomes,
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


class TestMiriSweepSharding(unittest.TestCase):
    @staticmethod
    def baseline_targets():
        path = os.path.join(os.path.dirname(__file__), "miri_baseline.json")
        with open(path, "r", encoding="utf-8") as f:
            return json.load(f)["targets"]

    def test_shards_union_covers_every_baseline_target_exactly_once(self):
        targets = self.baseline_targets()
        self.assertEqual(len(targets), len(set(targets)))
        for count in range(1, 6):
            shards = [shard_targets(targets, i, count) for i in range(1, count + 1)]
            flat = [t for shard in shards for t in shard]
            self.assertEqual(sorted(flat), sorted(targets), f"count={count}")
            self.assertEqual(len(flat), len(set(flat)), f"count={count}")

    def test_shards_are_balanced_and_deterministic(self):
        targets = self.baseline_targets()
        sizes = [len(shard_targets(targets, i, 2)) for i in (1, 2)]
        self.assertLessEqual(abs(sizes[0] - sizes[1]), 1)
        self.assertEqual(shard_targets(targets, 1, 2), shard_targets(list(reversed(targets)), 1, 2))

    def test_parse_shard_accepts_valid_and_rejects_invalid(self):
        self.assertEqual(parse_shard("1/2"), (1, 2))
        self.assertEqual(parse_shard("2/2"), (2, 2))
        for bad in ("0/2", "3/2", "1/0", "a/2", "1", "-1/2"):
            with self.assertRaises(argparse.ArgumentTypeError, msg=bad):
                parse_shard(bad)


if __name__ == "__main__":
    unittest.main()
