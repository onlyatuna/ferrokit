"""Run test targets under Miri and report one result per test, on the host or on a cross target.

A test that hits undefined behavior, or an operation Miri cannot run, stops the whole Miri run, so
a plain `cargo miri test` names only the first problem of a binary. This runs a binary in one go;
when a test stops the run, that test is recorded and the run starts again without the tests that
are already accounted for. Per test the raw kind is one of: pass, ignored, UB (undefined behavior),
panic, unsupported, deadlock, leak, timeout, notfound, error.

Summary buckets group raw kinds without ever counting environment failures as passes:
- `pass`: `pass`
- `ignored`: `ignored`
- `UB`: `UB` (separated into intentional negative controls and open defects when a baseline is given)
- `panic`: `panic`, `deadlock`, `leak`
- `env_fail`: `unsupported`, `timeout`, `notfound`, `error`

Both aliasing models run: Stacked Borrows (Miri's default, "SB") and Tree Borrows ("TB").
Disabling Stacked Borrows (`-Zmiri-disable-stacked-borrows`) or globally disabling isolation
(`-Zmiri-disable-isolation` in `--flags`) is refused. Per-test `-Zmiri-disable-isolation` is
allowed only through `--run-isolation-allowlist` for tests explicitly listed with a non-empty
`reason` in the baseline file, and those runs are reported separately from isolated runs.

Reading a result:
- `unsupported` / `env_fail` means Miri did not run that code. It is not a pass.
- A cross target (for example `x86_64-unknown-linux-gnu`) runs that target's dispatcher and OS
  calls. A pass there says nothing about the Windows dispatcher, real timers, or Windows FFI.
- `dyn QObject` is not `Unpin`, so Miri does not check the uniqueness of `&mut dyn QObject` or
  `Box<dyn QObject>`; a pass through them is no evidence. Test the same use on a concrete type.

Usage (from the repository root; a nightly toolchain with the `miri` component):
    python tools/miri_sweep.py [--crate ferrokit-core] [--targets lib,test_signal_owner_binding]
        [--cross x86_64-unknown-linux-gnu] [--toolchain nightly] [--models SB,TB]
        [--timeout SECONDS] [--flags "..."] [--baseline tools/miri_baseline.json]
        [--run-isolation-allowlist] [--fail-on-regression] [--shard I/N] [--list-targets]
        [--summary summary.md] [--out results.json] [--check-coverage]

`--check-coverage` runs no Miri. Every `tests/*.rs` file of the baseline's crate must be either in its
`targets` (swept) or in its `excluded` map ({target: reason}, reason non-empty). A file in neither, a
target in both, or an entry with no test file fails.
"""

import argparse
import json
import os
import re
import subprocess
import sys
import time

# Child processes must not open a console window when this runs from a console-less Windows Python.
NO_WINDOW = getattr(subprocess, "CREATE_NO_WINDOW", 0) if os.name == "nt" else 0

# Name of the per-target pseudo-test that records allocations leaked at process exit.
LEAK_PSEUDO_TEST = "(leaks at process exit)"


DETERMINISTIC_FLOATS = "-Zmiri-deterministic-floats"

FORBIDDEN_FLAGS = {
    "-Zmiri-disable-stacked-borrows",
    "-Zmiri-disable-isolation",
}

ALLOWED_STATUS_TRANSITIONS = {
    # Baseline expected status -> set of allowed current statuses (excluding identity exp == cur).
    # A test in baseline can only transition to 'pass' (an improvement), or stay the same.
    # Any other transition (e.g. panic -> timeout, unsupported -> panic, panic -> deadlock) is a regression.
    "pass": set(),
    "ignored": {"pass"},
    "unsupported": {"pass"},
    "panic": {"pass"},
    "deadlock": {"pass"},
    "leak": {"pass"},
    "timeout": {"pass"},
    "error": {"pass"},
    "UB": {"pass"},
}
def target_args(target):
    return ["--lib"] if target == "lib" else ["--test", target]


def bucket_of(kind):
    if kind == "pass":
        return "pass"
    if kind == "ignored":
        return "ignored"
    if kind == "UB":
        return "UB"
    if kind in ("panic", "deadlock", "leak"):
        return "panic"
    return "env_fail"


def list_tests(args, target):
    cmd = ["cargo", f"+{args.toolchain}", "miri", "test", "--locked", "-p", args.crate]
    if args.cross:
        cmd += ["--target", args.cross]
    cmd += target_args(target) + ["--", "--list", "--format", "terse"]
    p = subprocess.run(
        cmd,
        creationflags=NO_WINDOW,
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
    )
    if p.returncode != 0:
        print(f"list failed for {target}: {p.stderr[-400:]}", flush=True)
    return [line[: -len(": test")] for line in p.stdout.splitlines() if line.endswith(": test")]


def normalize(text):
    text = re.sub(r"<\d+>", "<N>", text)
    text = re.sub(r"alloc\d+", "allocN", text)
    text = re.sub(r"0x[0-9a-f]+", "0xX", text)
    return text.strip()


def classify(text):
    """Result kind and a signature (error line, then the source locations Miri names)."""
    m = re.search(r"^error: (.*)$", text, re.M)
    if m and "Undefined Behavior" in m.group(1):
        kind = "UB"
    elif m and ("unsupported operation" in m.group(1) or "can't call foreign function" in m.group(1)):
        kind = "unsupported"
    elif m and "deadlock" in m.group(1).lower():
        kind = "deadlock"
    elif m and "leaked" in m.group(1):
        kind = "leak"
    elif re.search(r"panicked at", text):
        kind = "panic"
    elif m:
        kind = "error"
    else:
        kind = "panic"
    sig = []
    if m:
        sig.append(("error", normalize(m.group(1))[:140]))
    role = "error"
    for line in (text[m.end():] if m else text).splitlines():
        if "was created by" in line:
            role = "created"
        elif "later invalidated" in line:
            role = "invalidated"
        loc = re.match(r"\s*--> (\S+?):(\d+):\d+", line)
        if loc and len(sig) < 6:
            sig.append((role, f"{loc.group(1).replace(chr(92), '/')}:{loc.group(2)}"))
        if line.startswith("note: some details"):
            break
    if kind == "panic":
        pm = re.search(r"panicked at ([^\n]+)\n([^\n]*)", text)
        sig.append(("panic", normalize(pm.group(1) + " " + pm.group(2))[:160] if pm else ""))
    return kind, sig


def validate_flags(raw_flags):
    env_flags = os.environ.get("MIRIFLAGS", "")
    tokens = raw_flags.split() + env_flags.split()
    for tok in tokens:
        if tok in FORBIDDEN_FLAGS:
            raise ValueError(
                f"Forbidden flag `{tok}` in --flags or MIRIFLAGS: aliasing checks must stay enabled "
                f"and `-Zmiri-disable-isolation` is only allowed per-test via --run-isolation-allowlist."
            )
    return raw_flags.split()


def run_miri(args, target, model, skip, exact_test=None, extra_miri_flags=None):
    env = dict(os.environ)
    # With isolation disabled the program sees the host environment: a host Wayland/X11 session
    # (for example WSLg) would make window creation connect to it and Miri cannot run that. CI has
    # no display, so drop these to get the same result everywhere.
    for var in ("WAYLAND_DISPLAY", "DISPLAY"):
        env.pop(var, None)
    # Miri randomises some float results by seed (a NaN `min`/`max`, a multiply-add that may be
    # fused). Rendering code that sits on a rounding tie (tiny-skia strokes at half-pixel
    # coordinates) then changes with how much randomness earlier code consumed, so adding an
    # unrelated test flipped `check_mark_matches_qt_pixels`. Native builds are deterministic.
    flags = [DETERMINISTIC_FLOATS] + validate_flags(args.flags)
    if extra_miri_flags:
        flags.extend(extra_miri_flags)
    if model == "TB":
        flags.append("-Zmiri-tree-borrows")
    if flags:
        env["MIRIFLAGS"] = " ".join(flags)
    else:
        env.pop("MIRIFLAGS", None)
    cmd = ["cargo", f"+{args.toolchain}", "miri", "test", "--locked", "-p", args.crate]
    if args.cross:
        cmd += ["--target", args.cross]
    cmd += target_args(target) + ["--", "--test-threads=1"]
    if exact_test is not None:
        cmd += ["--exact", exact_test]
    elif skip:
        cmd.append("--exact")
        for name in sorted(skip):
            cmd += ["--skip", name]
    try:
        p = subprocess.run(
            cmd,
            env=env,
            capture_output=True,
            text=True,
            timeout=args.timeout,
            encoding="utf-8",
            errors="replace",
            creationflags=NO_WINDOW,
        )
        return p.stdout + "\n" + p.stderr, False
    except subprocess.TimeoutExpired as e:
        parts = [(x.decode("utf-8", "replace") if isinstance(x, bytes) else x or "") for x in (e.stdout, e.stderr)]
        return "\n".join(parts), True


# libtest tags `#[should_panic]` tests as `test <name> - should panic ... <outcome>`. The bare name is
# the key; the suffix is only a marker.
HEAD_RE = re.compile(r"^test (\S+)(?: - should panic)? \.\.\.(?: (.*))?$")
OUTCOME_TAIL_RE = re.compile(r"(ok|FAILED|ignored)(?:, .*)?")
# libtest can print stderr between `test <name> ... ` and its outcome. The outcome then lands on a
# bare line, which belongs to the most recent start that still has no outcome.
CONTINUATION_RE = re.compile(r"(ok|FAILED|ignored)(?:, .*)?")


def scan_outcomes(text):
    """Return (outcomes, unresolved): (name, outcome) pairs, and names started with no outcome."""
    starts = []  # [name, outcome-or-None], in start order
    unresolved_idx = []  # indexes into starts that still have no outcome, in start order
    for line in text.splitlines():
        line = line.rstrip()
        head = HEAD_RE.match(line)
        if head:
            tail = OUTCOME_TAIL_RE.fullmatch(head.group(2) or "")
            starts.append([head.group(1), tail.group(1) if tail else None])
            if tail is None:
                unresolved_idx.append(len(starts) - 1)
            continue
        cont = CONTINUATION_RE.fullmatch(line)
        if cont and unresolved_idx:
            starts[unresolved_idx.pop()][1] = cont.group(1)
    outcomes = [(name, outcome) for name, outcome in starts if outcome is not None]
    unresolved = [name for name, outcome in starts if outcome is None]
    return outcomes, unresolved


def isolation_outcome(text, test_name):
    """Outcome (ok/FAILED/ignored) libtest reported for `test_name` in `text`, or None."""
    for name, outcome in scan_outcomes(text)[0]:
        if name == test_name:
            return outcome
    return None


def failure_section(text, name):
    m = re.search(
        r"^---- " + re.escape(name) + r"(?: - should panic)? stdout ----\n(.*?)(?=^---- |\nfailures:\n|\Z)",
        text,
        re.M | re.S,
    )
    return m.group(1) if m else ""


def last_start(text, name):
    starts = [m.start() for m in re.finditer(r"^test " + re.escape(name) + r"(?: - should panic)? \.\.\. ", text, re.M)]
    return starts[-1] if starts else -1


def sweep_target(args, target, names, model, results, excerpts):
    done = set()
    started = time.time()
    for _ in range(len(names) + 3):
        if len(done) == len(names):
            break
        text, timed_out = run_miri(args, target, model, done)
        outcomes, unresolved = scan_outcomes(text)
        for name, outcome in outcomes:
            if name in done or name not in names:
                continue
            key = f"{target}::{name}::{model}"
            if outcome == "FAILED":
                section = failure_section(text, name)
                if section:
                    kind, sig = classify(section)
                else:
                    # libtest prints a failed test's output only when the run ends normally. A
                    # later stop (undefined behavior) in the same run cuts it off, and that stop
                    # belongs to another test, so it must not be read as this test's cause.
                    kind, sig = "panic", [("panic", "failed; its output was cut off by a later stop")]
                results[key] = {"kind": kind, "bucket": bucket_of(kind), "sig": sig}
                excerpts[key] = section[:3000]
            else:
                kind = "pass" if outcome == "ok" else "ignored"
                results[key] = {"kind": kind, "bucket": bucket_of(kind), "sig": None}
            done.add(name)
        # A test that started and never reported is what stopped the run.
        started_names = unresolved
        stopper = next((n for n in reversed(started_names) if n in names and n not in done), None)
        if stopper:
            tail = text[last_start(text, stopper) :]
            key = f"{target}::{stopper}::{model}"
            if timed_out:
                results[key] = {"kind": "timeout", "bucket": "env_fail", "sig": None}
            else:
                kind, sig = classify(tail)
                results[key] = {"kind": kind, "bucket": bucket_of(kind), "sig": sig}
            excerpts[key] = tail[:3000]
            done.add(stopper)
            print(f"{target}::{stopper} {model} {results[key]['kind']}", flush=True)
            continue
        if re.search(r"^test result: ", text, re.M):
            # Miri reports leaks after the last test line, when the process ends, so every test
            # shows `ok` and only the exit status fails. The leak cannot be tied to one test, so
            # it is recorded under its own key: a baseline never lists it, which makes it a
            # regression instead of letting the target count as all-pass.
            leaked = len(re.findall(r"^error: memory leaked", text, re.M))
            if leaked:
                key = f"{target}::{LEAK_PSEUDO_TEST}::{model}"
                results[key] = {
                    "kind": "leak",
                    "bucket": bucket_of("leak"),
                    "sig": [("error", f"{leaked} leaked allocations at process exit")],
                }
                excerpts[key] = text[text.find("error: memory leaked") :][:3000]
                print(f"{target} {model}: {leaked} leaked allocations at process exit", flush=True)
            break
        # Nothing reported and nothing started: the run itself failed (build error, no such test), or
        # it hit the time limit before any test started.
        kind, sig = ("timeout", None) if timed_out else classify(text)
        for name in names:
            if name not in done:
                key = f"{target}::{name}::{model}"
                final_kind = "notfound" if "running 0 tests" in text else kind
                results[key] = {"kind": final_kind, "bucket": bucket_of(final_kind), "sig": sig}
                excerpts[key] = text[-2000:]
                done.add(name)
        break
    counts = {}
    for name in names:
        kind = results.get(f"{target}::{name}::{model}", {"kind": "missing"})["kind"]
        counts[kind] = counts.get(kind, 0) + 1
    print(f"{target} {model}: {len(names)} tests {counts} in {round(time.time() - started)}s", flush=True)


def run_isolation_allowlist(args, allowlist, swept_targets, models):
    iso_results = {}
    for item_key, meta in sorted(allowlist.items()):
        reason = (meta.get("reason") or "").strip()
        if not reason:
            raise ValueError(f"Isolation allowlist entry `{item_key}` is missing a non-empty `reason`.")
        target, test_name = item_key.split("::", 1)
        if target not in swept_targets:
            continue
        for model in models:
            text, timed_out = run_miri(
                args,
                target,
                model,
                skip=set(),
                exact_test=test_name,
                extra_miri_flags=["-Zmiri-disable-isolation"],
            )
            key = f"{target}::{test_name}::{model}"
            outcome = isolation_outcome(text, test_name)
            if timed_out:
                kind, sig = "timeout", None
            elif outcome == "ok":
                kind, sig = "pass", None
            elif outcome == "ignored":
                kind, sig = "ignored", None
            elif outcome == "FAILED":
                sec = failure_section(text, test_name) or text
                kind, sig = classify(sec)
            else:
                kind, sig = classify(text)
            iso_results[key] = {
                "kind": kind,
                "bucket": bucket_of(kind),
                "sig": sig,
                "isolation_disabled": True,
                "reason": reason,
            }
            print(f"[isolation-disabled] {key}: {kind}", flush=True)
    return iso_results


def compare_with_baseline(results, iso_results, baseline, swept_targets, models):
    base_res = baseline.get("results", {})
    controls = set(baseline.get("negative_controls", []))
    allowlist = baseline.get("isolation_allowlist", {})
    regressions = []
    improvements = []

    for key, cur in sorted(results.items()):
        cur_kind = cur["kind"]
        test_id = key.rsplit("::", 1)[0]
        exp_kind = base_res.get(key)
        if test_id in controls and exp_kind == "UB":
            if cur_kind != "UB":
                regressions.append(
                    f"{key}: negative control expected UB, got {cur_kind}"
                )
            continue
        if exp_kind is None:
            if cur_kind not in ("pass", "ignored"):
                regressions.append(f"{key}: new test not in baseline failed with {cur_kind}")
            continue
        if cur_kind == exp_kind:
            continue

        allowed = ALLOWED_STATUS_TRANSITIONS.get(exp_kind, {"pass"})
        if cur_kind in allowed:
            if cur_kind == "pass":
                improvements.append(f"{key}: baseline {exp_kind} -> pass")
        else:
            regressions.append(f"{key}: baseline {exp_kind} -> {cur_kind}")
    for key, exp_kind in sorted(base_res.items()):
        target = key.split("::", 1)[0]
        model = key.rsplit("::", 1)[-1]
        if target in swept_targets and model in models and key not in results:
            if exp_kind == "leak" and f"::{LEAK_PSEUDO_TEST}::" in key:
                improvements.append(f"{key}: baseline leak -> no leak")
            else:
                regressions.append(f"{key}: baseline test missing from sweep (expected {exp_kind})")

    for item_key, meta in sorted(allowlist.items()):
        target = item_key.split("::", 1)[0]
        if target not in swept_targets:
            continue
        exp_by_model = meta.get("expected", {})
        for model in models:
            full_key = f"{item_key}::{model}"
            if full_key not in iso_results:
                continue
            cur_kind = iso_results[full_key]["kind"]
            exp_kind = exp_by_model.get(model)
            if exp_kind and cur_kind != exp_kind:
                if exp_kind == "pass" or cur_kind == "UB":
                    regressions.append(
                        f"[isolation-disabled] {full_key}: expected {exp_kind}, got {cur_kind}"
                    )

    return regressions, improvements


def format_summary(args, models, results, iso_results, baseline, regressions, improvements):
    controls = set((baseline or {}).get("negative_controls", []))
    lines = [
        "## Nightly Miri Summary",
        "",
        f"- Crate: `{args.crate}`",
        f"- Target: `{args.cross or 'host'}`",
        f"- Toolchain: `{args.toolchain}`",
        f"- Isolation: enabled by default (`-Zmiri-disable-isolation` forbidden in global flags)",
        "",
        "| Model | Total | Pass | UB (Open Defect) | UB (Negative Control) | Panic / Timeout | Env / Unsupported (Not a Pass) |",
        "|---|---:|---:|---:|---:|---:|---:|",
    ]
    for model in models:
        m_items = [(k, v) for k, v in results.items() if k.endswith(f"::{model}")]
        total = len(m_items)
        n_pass = sum(1 for _, v in m_items if v["bucket"] == "pass")
        n_ctrl_ub = sum(
            1 for k, v in m_items if v["kind"] == "UB" and k.rsplit("::", 1)[0] in controls
        )
        n_open_ub = sum(
            1 for k, v in m_items if v["kind"] == "UB" and k.rsplit("::", 1)[0] not in controls
        )
        n_panic = sum(1 for _, v in m_items if v["bucket"] == "panic")
        n_env = sum(1 for _, v in m_items if v["bucket"] == "env_fail")
        lines.append(
            f"| `{model}` | {total} | {n_pass} | {n_open_ub} | {n_ctrl_ub} | {n_panic} | {n_env} |"
        )

    if iso_results:
        lines += [
            "",
            "### Per-Test `-Zmiri-disable-isolation` Allowlist Results (Counted Separately)",
            "",
            "| Test | SB | TB | Recorded Reason |",
            "|---|---|---|---|",
        ]
        allowlist = (baseline or {}).get("isolation_allowlist", {})
        for item_key, meta in sorted(allowlist.items()):
            if f"{item_key}::SB" not in iso_results and f"{item_key}::TB" not in iso_results:
                continue
            sb = iso_results.get(f"{item_key}::SB", {}).get("kind", "n/a")
            tb = iso_results.get(f"{item_key}::TB", {}).get("kind", "n/a")
            lines.append(f"| `{item_key}` | `{sb}` | `{tb}` | {meta.get('reason', '')} |")

    if baseline is not None:
        lines += [
            "",
            f"- Baseline regressions: **{len(regressions)}**",
            f"- Baseline improvements: **{len(improvements)}**",
        ]
        if regressions:
            lines.append("")
            lines.append("#### Regressions")
            for r in regressions:
                lines.append(f"- `{r}`")
        if improvements:
            lines.append("")
            lines.append("#### Improvements")
            for imp in improvements:
                lines.append(f"- `{imp}`")

    return "\n".join(lines) + "\n"


def parse_shard(text):
    """Parse `I/N` (1-based shard index I of N shards)."""
    try:
        index_str, count_str = text.split("/", 1)
        index, count = int(index_str), int(count_str)
    except ValueError:
        raise argparse.ArgumentTypeError(f"expected I/N, got {text!r}")
    if count < 1 or not 1 <= index <= count:
        raise argparse.ArgumentTypeError(f"shard {text!r} must satisfy 1 <= I <= N")
    return index, count


# Seconds assumed for a target that has no recorded time (a new test file).
DEFAULT_TARGET_SECONDS = 30
WEIGHTS_PATH = os.path.join(os.path.dirname(os.path.abspath(__file__)), "miri_target_weights.json")


def load_target_weights(crate, models, path=WEIGHTS_PATH):
    """Recorded Miri seconds per target of `crate`, summed over `models`; {} when none are recorded.

    The file maps crate -> target -> model -> seconds, measured on CI. The numbers only balance the
    shards; a stale or missing entry changes how long a shard takes, never which tests run.
    """
    try:
        with open(path, "r", encoding="utf-8") as f:
            recorded = json.load(f).get(crate, {})
    except FileNotFoundError:
        return {}
    return {t: sum(per_model.get(m, 0) for m in models) for t, per_model in recorded.items()}


def shard_targets(names, index, count, weights=None):
    """Deterministic partition of `names` into `count` shards; returns shard `index` (1-based).

    Longest-processing-time first: targets go heaviest first, each to the currently lightest shard
    (ties: lower shard number). `weights` maps target -> seconds; unknown targets weigh
    DEFAULT_TARGET_SECONDS. Without weights every target weighs the same and this is round-robin
    over the sorted names. The result depends only on the set of names and the weights.
    """
    weights = weights or {}
    order = sorted(names, key=lambda n: (-weights.get(n, DEFAULT_TARGET_SECONDS), n))
    load = [0] * count
    shards = [[] for _ in range(count)]
    for name in order:
        lightest = min(range(count), key=lambda i: (load[i], i))
        load[lightest] += weights.get(name, DEFAULT_TARGET_SECONDS)
        shards[lightest].append(name)
    return sorted(shards[index - 1])


REPO_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def crate_test_targets(crate, repo_root=REPO_ROOT):
    """Integration test targets of `crate`: the stems of the `tests/*.rs` files directly in it."""
    tests_dir = os.path.join(repo_root, "crates", crate, "tests")
    return {
        os.path.splitext(name)[0]
        for name in os.listdir(tests_dir)
        if name.endswith(".rs") and os.path.isfile(os.path.join(tests_dir, name))
    }


def check_target_coverage(baseline, test_names):
    """Problems with how `baseline` accounts for the integration test targets in `test_names`.

    Each test target is either swept (`targets`) or excluded with a reason (`excluded`, a map of
    target to reason). Returns a list of messages; an empty list means the baseline is complete.
    """
    excluded = baseline.get("excluded", {})
    if not isinstance(excluded, dict):
        return ["`excluded` must be an object mapping target to reason"]
    problems = []
    swept = set(baseline.get("targets", []))
    for name in sorted(test_names - swept - set(excluded)):
        problems.append(f"tests/{name}.rs is in neither `targets` nor `excluded`")
    for name in sorted(swept & set(excluded)):
        problems.append(f"{name} is in both `targets` and `excluded`")
    for name, reason in sorted(excluded.items()):
        if not isinstance(reason, str) or not reason.strip():
            problems.append(f"excluded {name} needs a non-empty reason")
    listed = (swept - {"lib"}) | set(excluded)
    for name in sorted(listed - test_names):
        problems.append(f"{name} is listed but tests/{name}.rs does not exist")
    return problems


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--crate", default="ferrokit-core")
    ap.add_argument(
        "--targets",
        help="comma list of integration test names, or `lib` (defaults to baseline targets when --baseline is set)",
    )
    ap.add_argument("--cross", help="cross target triple, e.g. x86_64-unknown-linux-gnu")
    ap.add_argument("--toolchain", default="nightly")
    ap.add_argument("--models", default="SB,TB")
    ap.add_argument("--timeout", type=int, default=10800, help="seconds for one Miri run")
    ap.add_argument("--flags", default="", help="extra MIRIFLAGS (none by default)")
    ap.add_argument("--baseline", help="path to baseline JSON for regression comparison")
    ap.add_argument(
        "--run-isolation-allowlist",
        action="store_true",
        help="also run the per-test -Zmiri-disable-isolation allowlist from --baseline",
    )
    ap.add_argument(
        "--fail-on-regression",
        action="store_true",
        help="exit with code 1 if any baseline regression or negative-control failure occurs",
    )
    ap.add_argument("--summary", help="write Markdown summary to this file")
    ap.add_argument(
        "--shard",
        type=parse_shard,
        metavar="I/N",
        help="run only shard I (1-based) of N; shards partition the targets, balanced by tools/miri_target_weights.json",
    )
    ap.add_argument(
        "--list-targets",
        action="store_true",
        help="print the target list selected by --targets/--baseline/--shard and exit without running Miri",
    )
    ap.add_argument(
        "--check-coverage",
        action="store_true",
        help="check that every tests/*.rs of --crate is in the baseline `targets` or `excluded`; run no Miri",
    )
    ap.add_argument("--out", help="write the results as JSON")
    args = ap.parse_args()

    validate_flags(args.flags)
    baseline = None
    if args.baseline:
        with open(args.baseline, "r", encoding="utf-8") as f:
            baseline = json.load(f)

    if baseline is not None and baseline.get("crate") not in (None, args.crate):
        ap.error(f"--baseline is for crate {baseline['crate']!r}, but --crate is {args.crate!r}")

    if args.check_coverage:
        if baseline is None:
            ap.error("--check-coverage requires --baseline")
        problems = check_target_coverage(baseline, crate_test_targets(args.crate))
        for problem in problems:
            print(f"coverage: {problem}")
        if problems:
            print(f"{len(problems)} target coverage problem(s) in {args.baseline}")
            return 1
        print(f"coverage: every {args.crate} test target is in `targets` or `excluded`")
        return 0

    targets_str = args.targets
    if not targets_str:
        if baseline and "targets" in baseline:
            targets_str = ",".join(baseline["targets"])
        else:
            ap.error("--targets is required when --baseline does not specify `targets`")

    target_list = [t.strip() for t in targets_str.split(",") if t.strip()]
    if args.shard:
        index, count = args.shard
        shard_models = [m.strip() for m in args.models.split(",") if m.strip()]
        target_list = shard_targets(
            target_list, index, count, load_target_weights(args.crate, shard_models)
        )
        if not target_list:
            ap.error(f"--shard {index}/{count} selects no targets")
    if args.list_targets:
        print("\n".join(target_list))
        return 0
    swept_targets = set(target_list)
    models = [m.strip() for m in args.models.split(",") if m.strip()]
    results, excerpts, tests = {}, {}, {}
    for target in target_list:
        tests[target] = list_tests(args, target)
        for model in models:
            sweep_target(args, target, tests[target], model, results, excerpts)

    iso_results = {}
    if args.run_isolation_allowlist:
        if not baseline or "isolation_allowlist" not in baseline:
            ap.error("--run-isolation-allowlist requires --baseline with `isolation_allowlist`")
        iso_results = run_isolation_allowlist(
            args, baseline["isolation_allowlist"], swept_targets, models
        )

    regressions, improvements = [], []
    if baseline is not None:
        regressions, improvements = compare_with_baseline(
            results, iso_results, baseline, swept_targets, models
        )

    summary_md = format_summary(args, models, results, iso_results, baseline, regressions, improvements)
    print("\n" + summary_md, flush=True)
    if args.summary:
        with open(args.summary, "a", encoding="utf-8") as f:
            f.write(summary_md)

    if args.out:
        with open(args.out, "w", encoding="utf-8") as f:
            json.dump(
                {
                    "crate": args.crate,
                    "cross": args.cross,
                    "toolchain": args.toolchain,
                    "models": models,
                    "tests": tests,
                    "results": results,
                    "isolation_results": iso_results,
                    "regressions": regressions,
                    "improvements": improvements,
                    "excerpts": excerpts,
                },
                f,
                indent=1,
            )

    if args.fail_on_regression and regressions:
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
