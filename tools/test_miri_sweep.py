import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(__file__))
from miri_sweep import compare_with_baseline, ALLOWED_STATUS_TRANSITIONS, OUTCOME_RE, failure_section
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

    def test_should_panic_outcome_lines_parse_to_bare_name(self):
        text = (
            "test a::plain ... ok\n"
            "test a::boom - should panic ... ok\n"
            "test a::ign - should panic ... ignored\n"
            "test a::bad - should panic ... FAILED\n"
            "test a::skip ... ignored, reason\n"
        )
        self.assertEqual(
            OUTCOME_RE.findall(text),
            [("a::plain", "ok"), ("a::boom", "ok"), ("a::ign", "ignored"), ("a::bad", "FAILED"), ("a::skip", "ignored")],
        )

    def test_should_panic_failure_section_found_by_bare_name(self):
        text = "---- a::bad - should panic stdout ----\nmsg\n\nfailures:\n"
        self.assertEqual(failure_section(text, "a::bad").strip(), "msg")


if __name__ == "__main__":
    unittest.main()
