import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(__file__))
from miri_scope import build_matrix, select_crates

ALL = {"core", "gui", "widgets"}


def scope(event="pull_request", labels=(), changed=()):
    return select_crates(event, set(labels), list(changed))


class SelectCratesTests(unittest.TestCase):
    def test_widgets_only(self):
        self.assertEqual(scope(changed=["crates/ferrokit-widgets/src/a.rs"]), {"widgets"})
        self.assertEqual(scope(changed=["tools/miri_baseline_ferrokit_widgets.json"]), {"widgets"})

    def test_platform_runs_widgets(self):
        self.assertEqual(scope(changed=["crates/ferrokit-platform/src/a.rs"]), {"widgets"})

    def test_gui_runs_gui_and_widgets(self):
        self.assertEqual(scope(changed=["crates/ferrokit-gui/src/a.rs"]), {"gui", "widgets"})
        self.assertEqual(scope(changed=["tools/miri_baseline_ferrokit_gui.json"]), {"gui", "widgets"})

    def test_everything_else_runs_all(self):
        for path in [
            "crates/ferrokit-core/src/a.rs",
            "tools/miri_baseline.json",
            "Cargo.lock",
            "Cargo.toml",
            ".cargo/config.toml",
            "crates/ferrokit-derive/src/lib.rs",
            "tools/miri_sweep.py",
            "tools/test_miri_sweep.py",
            "tools/miri_scope.py",
            ".github/workflows/ci.yml",
            "rust-toolchain.toml",
        ]:
            with self.subTest(path=path):
                self.assertEqual(scope(changed=[path]), ALL)

    def test_docs_only_runs_nothing(self):
        self.assertEqual(scope(changed=["README.md", "docs/a.md"]), set())

    def test_mixed_paths_union(self):
        self.assertEqual(
            scope(changed=["README.md", "crates/ferrokit-widgets/a.rs", "crates/ferrokit-gui/b.rs"]),
            {"gui", "widgets"},
        )

    def test_full_label_schedule_dispatch_run_all(self):
        self.assertEqual(scope(labels=["miri-full"], changed=["README.md"]), ALL)
        self.assertEqual(scope(labels=["miri-full"], changed=["crates/ferrokit-widgets/a.rs"]), ALL)
        self.assertEqual(scope(event="schedule"), ALL)
        self.assertEqual(scope(event="workflow_dispatch"), ALL)

    def test_miri_label_keeps_derived_crates(self):
        self.assertEqual(scope(labels=["miri"], changed=["crates/ferrokit-widgets/a.rs"]), {"widgets"})

    def test_miri_label_without_derivable_crate_runs_all(self):
        self.assertEqual(scope(labels=["miri"], changed=["README.md"]), ALL)

    def test_push_runs_nothing(self):
        self.assertEqual(scope(event="push", changed=["crates/ferrokit-core/a.rs"]), set())


class BuildMatrixTests(unittest.TestCase):
    def test_empty(self):
        self.assertEqual(build_matrix(set()), {"include": []})

    def test_all_shard_counts(self):
        include = build_matrix(ALL)["include"]
        self.assertEqual(len(include), 10)
        for model in ("SB", "TB"):
            for crate, shards in [("ferrokit-core", 2), ("ferrokit-gui", 1), ("ferrokit-widgets", 2)]:
                got = [e["shard"] for e in include if e["crate"] == crate and e["model"] == model]
                self.assertEqual(got, list(range(1, shards + 1)))
                self.assertTrue(all(e["shards"] == shards for e in include if e["crate"] == crate))

    def test_widgets_only_matrix(self):
        include = build_matrix({"widgets"})["include"]
        self.assertEqual({e["crate"] for e in include}, {"ferrokit-widgets"})
        self.assertEqual(len(include), 4)


if __name__ == "__main__":
    unittest.main()
