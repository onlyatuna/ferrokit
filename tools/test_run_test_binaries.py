import contextlib
import io
import json
import os
import sys
import unittest
from unittest import mock

sys.path.insert(0, os.path.dirname(__file__))
import run_test_binaries


def artifact(kind, name, package_dir, test_profile=True):
    return json.dumps(
        {
            "reason": "compiler-artifact",
            "executable": "C:/repo/target/debug/deps/" + name + ".exe",
            "profile": {"test": test_profile},
            "target": {"kind": [kind], "name": name},
            "manifest_path": "C:/repo/crates/" + package_dir + "/Cargo.toml",
        }
    )


class TestParseTestTargets(unittest.TestCase):
    def test_integration_test_runs_with_locked_and_its_name(self):
        found = run_test_binaries.parse_test_targets([artifact("test", "test_x", "ferrokit-core")], "1")
        self.assertEqual(
            found,
            [("ferrokit-core test_x", ["cargo", "test", "--locked", "-j", "1", "-p", "ferrokit-core", "--test", "test_x"])],
        )

    def test_library_target_uses_lib_flag_without_a_name(self):
        found = run_test_binaries.parse_test_targets([artifact("lib", "ferrokit-core", "ferrokit-core")], "1")
        self.assertEqual(
            found,
            [("ferrokit-core ferrokit-core", ["cargo", "test", "--locked", "-j", "1", "-p", "ferrokit-core", "--lib"])],
        )

    def test_skips_non_test_artifacts_and_unparseable_lines(self):
        lines = [
            "not json",
            artifact("test", "test_y", "ferrokit-core", test_profile=False),
            json.dumps({"reason": "build-finished", "success": True}),
            artifact("test", "test_z", "ferrokit-core"),
        ]
        found = run_test_binaries.parse_test_targets(lines, "2")
        self.assertEqual([name for name, _ in found], ["ferrokit-core test_z"])
        self.assertEqual(found[0][1][4], "2")

    def test_no_lines_yields_no_targets(self):
        self.assertEqual(run_test_binaries.parse_test_targets([], "1"), [])


class TestMainWithoutTargets(unittest.TestCase):
    def test_zero_targets_fails_instead_of_reporting_success(self):
        err = io.StringIO()
        with mock.patch.object(run_test_binaries, "test_targets", return_value=[]), mock.patch.object(
            sys, "argv", ["run_test_binaries.py"]
        ), contextlib.redirect_stderr(err), contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(run_test_binaries.main(), 1)
        self.assertIn("no test targets", err.getvalue())


if __name__ == "__main__":
    unittest.main()
