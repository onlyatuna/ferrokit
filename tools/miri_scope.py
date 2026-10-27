#!/usr/bin/env python3
"""Decide which crates' Miri shards a CI run needs, and print the shard matrix.

Usage: miri_scope.py --event EVENT [--labels-json '["miri"]'] < changed_files.txt
Prints `run=true|false` and `matrix={"include":[...]}` lines (append to $GITHUB_OUTPUT).

Dependents follow their dependencies: core -> gui -> widgets. Schedule, workflow_dispatch and
the `miri-full` label run every crate; a pull request otherwise runs the crates its changed
paths affect, and the `miri` label with no derivable crate runs every crate. Other events
(pushes to main) run nothing; the nightly run covers them.
"""

import argparse
import json
import sys

CRATES = ("core", "gui", "widgets")
MODELS = ("SB", "TB")
# crate -> (cargo package, baseline, shards per model)
SHARDS = {
    "core": ("ferrokit-core", "tools/miri_baseline.json", 2),
    "gui": ("ferrokit-gui", "tools/miri_baseline_ferrokit_gui.json", 4),
    "widgets": ("ferrokit-widgets", "tools/miri_baseline_ferrokit_widgets.json", 2),
}
# Exact-name or prefix rules, checked in order; first match wins. Value: crates to run.
CORE_DEPENDENTS = {"core", "gui", "widgets"}
GUI_DEPENDENTS = {"gui", "widgets"}
EXACT = {
    "tools/miri_baseline.json": CORE_DEPENDENTS,
    "tools/miri_baseline_ferrokit_gui.json": GUI_DEPENDENTS,
    "tools/miri_baseline_ferrokit_widgets.json": {"widgets"},
}
PREFIX = (
    ("crates/ferrokit-core/", CORE_DEPENDENTS),
    ("crates/ferrokit-gui/", GUI_DEPENDENTS),
    ("crates/ferrokit-widgets/", {"widgets"}),
    ("crates/ferrokit-platform/", {"widgets"}),
    ("crates/ferrokit-derive/", set(CRATES)),
    (".cargo/", set(CRATES)),
    ("tools/miri_", set(CRATES)),
    ("tools/test_miri_", set(CRATES)),
)
ALL_FILES = {
    ".github/workflows/ci.yml",
    "Cargo.toml",
    "Cargo.lock",
    "rust-toolchain",
    "rust-toolchain.toml",
}


def crates_for_path(path):
    if path in EXACT:
        return EXACT[path]
    if path in ALL_FILES:
        return set(CRATES)
    for prefix, crates in PREFIX:
        if path.startswith(prefix):
            return crates
    return set()


def select_crates(event, labels, changed):
    if event in ("schedule", "workflow_dispatch") or "miri-full" in labels:
        return set(CRATES)
    if event != "pull_request":
        return set()
    crates = set()
    for path in changed:
        crates |= crates_for_path(path.strip())
    if not crates and "miri" in labels:
        return set(CRATES)
    return crates


def build_matrix(crates):
    include = []
    for model in MODELS:
        for name in CRATES:
            if name not in crates:
                continue
            package, baseline, shards = SHARDS[name]
            for shard in range(1, shards + 1):
                include.append(
                    {
                        "crate": package,
                        "baseline": baseline,
                        "model": model,
                        "shard": shard,
                        "shards": shards,
                    }
                )
    return {"include": include}


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--event", required=True)
    ap.add_argument("--labels-json", default="[]")
    args = ap.parse_args(argv)
    labels = set(json.loads(args.labels_json) or [])
    changed = [line for line in sys.stdin.read().splitlines() if line.strip()]
    matrix = build_matrix(select_crates(args.event, labels, changed))
    print(f"run={'true' if matrix['include'] else 'false'}")
    print("matrix=" + json.dumps(matrix, separators=(",", ":")))
    return 0


if __name__ == "__main__":
    sys.exit(main())
