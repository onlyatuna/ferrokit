import contextlib
import io
import json
import os
import sys
import tempfile
import unittest
from unittest import mock

sys.path.insert(0, os.path.dirname(__file__))
import run_test_binaries

ROOT = os.path.join(os.sep, "repo")
TARGET = os.path.join(ROOT, "target", "debug")


def package(name, bins=(), **extra):
    manifest = os.path.join(ROOT, "crates", name, "Cargo.toml")
    pkg = {
        "name": name,
        "version": "1.2.3",
        "manifest_path": manifest,
        "targets": [{"kind": ["bin"], "name": b} for b in bins] + [{"kind": ["lib"], "name": name}],
    }
    pkg.update(extra)
    return pkg


def by_manifest(*packages):
    return {run_test_binaries.norm(p["manifest_path"]): p for p in packages}


def artifact(kind, name, pkg, test_profile=True):
    return json.dumps(
        {
            "reason": "compiler-artifact",
            "executable": os.path.join(TARGET, "deps", name + "-abc123"),
            "profile": {"test": test_profile},
            "target": {"kind": [kind], "name": name},
            "manifest_path": pkg["manifest_path"],
        }
    )


CORE = package("ferrokit-core")
WIDGETS = package("ferrokit-widgets")
TOOL = {"cargo": os.path.join(ROOT, "bin", "cargo"), "sysroot_libdir": os.path.join(ROOT, "sysroot", "lib"), "cargo_home": "/nonexistent"}


class TestParseTestTargets(unittest.TestCase):
    def test_integration_test_is_named_by_package_and_target(self):
        found = run_test_binaries.parse_test_targets([artifact("test", "test_x", CORE)], by_manifest(CORE))
        self.assertEqual([t["name"] for t in found], ["ferrokit-core test_x"])
        self.assertEqual(found[0]["executable"], os.path.join(TARGET, "deps", "test_x-abc123"))
        self.assertIs(found[0]["package"], CORE)
        self.assertTrue(found[0]["is_test"])

    def test_library_unit_tests_are_not_integration_tests(self):
        found = run_test_binaries.parse_test_targets([artifact("lib", "ferrokit_core", CORE)], by_manifest(CORE))
        self.assertEqual([t["name"] for t in found], ["ferrokit-core ferrokit_core"])
        self.assertFalse(found[0]["is_test"])

    def test_skips_non_test_artifacts_and_unparseable_lines(self):
        lines = [
            "not json",
            artifact("test", "test_y", CORE, test_profile=False),
            json.dumps({"reason": "build-finished", "success": True}),
            json.dumps({"reason": "compiler-artifact", "executable": None, "profile": {"test": True}}),
            artifact("test", "test_z", CORE),
        ]
        found = run_test_binaries.parse_test_targets(lines, by_manifest(CORE))
        self.assertEqual([t["name"] for t in found], ["ferrokit-core test_z"])

    def test_package_filter_keeps_only_named_packages(self):
        lines = [artifact("test", "a", CORE), artifact("test", "b", WIDGETS)]
        found = run_test_binaries.parse_test_targets(lines, by_manifest(CORE, WIDGETS), packages=["ferrokit-widgets"])
        self.assertEqual([t["name"] for t in found], ["ferrokit-widgets b"])

    def test_no_lines_yields_no_targets(self):
        self.assertEqual(run_test_binaries.parse_test_targets([], by_manifest(CORE)), [])


class TestCommand(unittest.TestCase):
    def test_runs_the_executable_itself(self):
        target = {"executable": "/t/x"}
        self.assertEqual(run_test_binaries.test_command(target), ["/t/x"])

    def test_test_threads_goes_straight_to_libtest(self):
        target = {"executable": "/t/x"}
        self.assertEqual(run_test_binaries.test_command(target, "1"), ["/t/x", "--test-threads", "1"])


def target_for(pkg, is_test=True):
    return {"name": "n", "executable": os.path.join(TARGET, "deps", "x-abc123"), "package": pkg, "is_test": is_test}


class TestEnvironment(unittest.TestCase):
    def test_package_variables_match_what_cargo_sets(self):
        pkg = package(
            "ferrokit-core",
            version="1.2.3-rc.1+build5",
            description="d",
            repository="r",
            license="L",
            authors=["a <a@x>", "b"],
            rust_version="1.90",
        )
        env = run_test_binaries.test_environment(target_for(pkg), {}, TOOL)
        self.assertEqual(env["CARGO_MANIFEST_DIR"], os.path.dirname(pkg["manifest_path"]))
        self.assertEqual(env["CARGO_MANIFEST_PATH"], pkg["manifest_path"])
        self.assertEqual(env["CARGO_PKG_NAME"], "ferrokit-core")
        self.assertEqual(env["CARGO_PKG_VERSION"], "1.2.3-rc.1+build5")
        self.assertEqual(
            [env["CARGO_PKG_VERSION_" + p] for p in ("MAJOR", "MINOR", "PATCH", "PRE")], ["1", "2", "3", "rc.1"]
        )
        self.assertEqual(env["CARGO_PKG_AUTHORS"], "a <a@x>:b")
        self.assertEqual(env["CARGO_PKG_RUST_VERSION"], "1.90")
        self.assertEqual(env["CARGO_PKG_HOMEPAGE"], "")
        self.assertEqual(env["CARGO"], TOOL["cargo"])

    def test_bin_exe_only_for_integration_tests(self):
        pkg = package("ferrokit-core", bins=["tool"])
        suffix = ".exe" if os.name == "nt" else ""
        expected = os.path.join(TARGET, "tool" + suffix)
        self.assertEqual(run_test_binaries.test_environment(target_for(pkg), {}, TOOL)["CARGO_BIN_EXE_tool"], expected)
        self.assertNotIn("CARGO_BIN_EXE_tool", run_test_binaries.test_environment(target_for(pkg, False), {}, TOOL))

    def test_library_path_gets_profile_deps_and_sysroot_in_front(self):
        var = run_test_binaries.dylib_path_var()
        env = run_test_binaries.test_environment(target_for(CORE), {var: "/old"}, TOOL)
        parts = env[var].split(os.pathsep)
        self.assertEqual(parts[:4], [TARGET, os.path.join(TARGET, "deps"), TOOL["sysroot_libdir"], "/old"][:4])

    def test_library_path_is_not_duplicated_when_already_in_front(self):
        var = run_test_binaries.dylib_path_var()
        once = run_test_binaries.test_environment(target_for(CORE), {var: "/old"}, TOOL)
        twice = run_test_binaries.test_environment(target_for(CORE), {var: once[var]}, TOOL)
        self.assertEqual(once[var], twice[var])

    def test_inherited_variables_are_kept(self):
        env = run_test_binaries.test_environment(target_for(CORE), {"KEEP": "1"}, TOOL)
        self.assertEqual(env["KEEP"], "1")


class TestConfigEnv(unittest.TestCase):
    def write_config(self, root, text):
        os.makedirs(os.path.join(root, ".cargo"))
        with open(os.path.join(root, ".cargo", "config.toml"), "w") as f:
            f.write(text)

    def test_unforced_entry_yields_to_the_process_environment(self):
        with tempfile.TemporaryDirectory() as root:
            self.write_config(root, '[env]\nRUST_TEST_THREADS = "1"\nOTHER = "x"\n')
            self.assertEqual(run_test_binaries.config_env(root, {}, "/nonexistent"), {"RUST_TEST_THREADS": "1", "OTHER": "x"})
            self.assertEqual(
                run_test_binaries.config_env(root, {"RUST_TEST_THREADS": "4"}, "/nonexistent"), {"OTHER": "x"}
            )

    def test_forced_entry_wins_and_relative_is_resolved_against_the_config_dir(self):
        with tempfile.TemporaryDirectory() as root:
            self.write_config(
                root, '[env]\nA = { value = "1", force = true }\nB = { value = "sub", relative = true }\n'
            )
            got = run_test_binaries.config_env(os.path.join(root, "deeper"), {"A": "0", "B": "keep"}, "/nonexistent")
            self.assertEqual(got, {"A": "1"})
            got = run_test_binaries.config_env(root, {}, "/nonexistent")
            self.assertEqual(got["B"], os.path.join(root, "sub"))

    def test_nearer_config_wins_over_cargo_home(self):
        with tempfile.TemporaryDirectory() as root, tempfile.TemporaryDirectory() as home:
            self.write_config(root, '[env]\nA = "near"\n')
            with open(os.path.join(home, "config.toml"), "w") as f:
                f.write('[env]\nA = "far"\nB = "home"\n')
            self.assertEqual(run_test_binaries.config_env(root, {}, home), {"A": "near", "B": "home"})

    def test_workspace_sets_a_single_test_thread(self):
        repo = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
        self.assertEqual(run_test_binaries.config_env(repo, {}, "/nonexistent").get("RUST_TEST_THREADS"), "1")


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
