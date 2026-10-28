#!/usr/bin/env python3
"""Unattended fuzz campaign over the `fuzz/` harnesses (cargo-fuzz, libFuzzer). Not part of CI.

For every target it runs libFuzzer for a time budget on several forked workers that keep going
after a finding, then collects the crash / timeout / oom inputs that are new, minimises each with
`cargo fuzz tmin`, replays the minimised input once to get the failure message, and prints a
report (Markdown on stdout, JSON with --json). Findings are deduplicated by failure message.

Nothing is written outside the git-ignored `fuzz/corpus/` and `fuzz/artifacts/`; the corpus is
never committed. A finding becomes a regression unit test in the crate that owns the code, with
the minimised input inlined as a byte string (the report prints it as hex when it is small).

Exit status: 0 no findings, 1 findings, 2 the campaign itself could not run.

    python tools/fuzz_campaign.py --seconds 600 --jobs 4
    python tools/fuzz_campaign.py --targets json_parse,variant --seconds 60 --json out.json
    python tools/fuzz_campaign.py --replay-only        # replay what is already in fuzz/artifacts
"""
import argparse
import hashlib
import json
import re
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
FUZZ = ROOT / "fuzz"
KINDS = ("crash-", "timeout-", "oom-", "leak-", "slow-unit-")
HEX_LIMIT = 256


def targets_from_manifest():
    text = (FUZZ / "Cargo.toml").read_text(encoding="utf-8")
    return re.findall(r'\[\[bin\]\]\s*name\s*=\s*"([^"]+)"', text)


def cargo(toolchain, *args, timeout):
    """Runs `cargo +toolchain fuzz ...` in the repo root; returns (returncode, combined output)."""
    cmd = ["cargo", f"+{toolchain}", "fuzz", *args]
    try:
        p = subprocess.run(
            cmd,
            cwd=ROOT,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            errors="replace",
            timeout=timeout,
        )
    except subprocess.TimeoutExpired as e:
        out = e.stdout or ""
        if isinstance(out, bytes):
            out = out.decode("utf-8", "replace")
        return 124, out
    return p.returncode, p.stdout


def artifacts(target):
    d = FUZZ / "artifacts" / target
    if not d.is_dir():
        return {}
    return {p.name: p for p in d.iterdir() if p.is_file()}


def failure_message(output):
    """The first line that says what went wrong, without addresses, so equal bugs compare equal."""
    for line in output.splitlines():
        if "panicked at" in line or "ERROR: libFuzzer" in line or "SUMMARY:" in line:
            return re.sub(r"\(\d+\)", "(N)", re.sub(r"0x[0-9a-f]+", "0x?", line.strip()))[:300]
    tail = [l for l in output.splitlines() if l.strip()]
    return (tail[-1] if tail else "no output")[:300]


def minimise(toolchain, target, path, tmin_seconds):
    """Returns `(minimised path or None, note)`; None means the input could not be minimised."""
    before = set(artifacts(target))
    rc, out = cargo(
        toolchain, "tmin", target, str(path), "--", f"-max_total_time={tmin_seconds}", timeout=tmin_seconds + 120
    )
    new = [n for n in set(artifacts(target)) - before if n.startswith("minimized-from-")]
    if new:
        return artifacts(target)[new[0]], "minimised"
    if rc == 124:
        return None, "minimisation timed out"
    if rc != 0:
        return None, f"cargo fuzz tmin failed (exit {rc})"
    # It ran and found nothing smaller: the input is already minimal.
    return path, "already minimal"


# What a replay prints when it fails the way a finding of that kind does.
MARKERS = {
    "crash": ("panicked at", "ERROR: AddressSanitizer", "deadly signal"),
    "timeout": ("ERROR: libFuzzer: timeout",),
    "oom": ("ERROR: libFuzzer: out-of-memory",),
    "leak": ("ERROR: LeakSanitizer",),
    "slow-unit": ("ERROR: libFuzzer: timeout",),
}


def replay(toolchain, target, path, kind):
    """Replays one input. Returns `(reproduced, why, output)`; `reproduced` is True when the run
    failed the way a finding of `kind` does, False when it ran clean, None when it proves nothing
    (outer timeout, the harness did not start, a failure of another sort)."""
    rc, out = cargo(toolchain, "run", target, str(path), "--", "-runs=1", "-timeout=10", timeout=300)
    if rc == 124:
        return None, "replay timed out", out
    if "INFO:" not in out:
        return None, f"the harness did not start (exit {rc})", out
    if rc == 0:
        return False, "replay ran clean", out
    markers = MARKERS.get(kind) or tuple(m for ms in MARKERS.values() for m in ms)
    if any(m in out for m in markers):
        return True, "replay failed as the finding did", out
    return None, f"replay exited {rc} without the expected {kind} failure", out


def investigate(toolchain, target, path, tmin_seconds):
    kind = next((k.rstrip("-") for k in KINDS if path.name.startswith(k)), "other")
    small, note = minimise(toolchain, target, path, tmin_seconds)
    used = small or path
    reproduced, why, out = replay(toolchain, target, used, kind)
    data = used.read_bytes()
    return {
        "target": target,
        "kind": kind,
        "artifact": str(path.relative_to(ROOT)),
        "minimised": str(small.relative_to(ROOT)) if small else None,
        "minimisation": note,
        "bytes": len(data),
        "sha1": hashlib.sha1(data).hexdigest(),
        "hex": data.hex() if len(data) <= HEX_LIMIT else None,
        "reproduces": reproduced,
        "replay": why,
        "message": failure_message(out) if reproduced else None,
    }


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--targets", help="comma list; default: every [[bin]] of fuzz/Cargo.toml")
    ap.add_argument("--seconds", type=int, default=300, help="libFuzzer time budget per target")
    ap.add_argument("--jobs", type=int, default=2, help="forked workers per target")
    ap.add_argument("--toolchain", default="nightly")
    ap.add_argument("--tmin-seconds", type=int, default=60, help="budget for minimising one finding")
    ap.add_argument("--replay-only", action="store_true", help="skip fuzzing; investigate existing artifacts")
    ap.add_argument("--json", help="also write the findings as JSON to this file")
    ap.add_argument("--slack-seconds", type=int, default=900, help="outer deadline = --seconds + this (build time)")
    args = ap.parse_args()

    known = targets_from_manifest()
    targets = args.targets.split(",") if args.targets else known
    unknown = [t for t in targets if t not in known]
    if unknown:
        print(f"unknown target(s) {unknown}; fuzz/Cargo.toml has {known}", file=sys.stderr)
        return 2

    findings, runs = [], []
    for t in targets:
        corpus = FUZZ / "corpus" / t
        corpus.mkdir(parents=True, exist_ok=True)
        seeds = FUZZ / "seeds" / t
        before = set(artifacts(t))
        started = time.time()
        if not args.replay_only:
            lf = [
                f"-max_total_time={args.seconds}",
                "-timeout=10",
                "-rss_limit_mb=2048",
                f"-fork={args.jobs}",
                "-ignore_crashes=1",
                "-ignore_timeouts=1",
                "-ignore_ooms=1",
            ]
            extra = [str(seeds)] if seeds.is_dir() else []
            rc, out = cargo(
                args.toolchain, "run", t, str(corpus), *extra, "--", *lf, timeout=args.seconds + args.slack_seconds
            )
            # A clean exit, or 77 (libFuzzer's "a finding was reported"), is a campaign that ran.
            # Anything else is one that did not finish: a build failure, a missing toolchain, a hang
            # past the outer deadline (exit 124), a crash of the fuzzer itself. It must never look
            # like "no findings".
            if rc not in (0, 77) or "INFO:" not in out:
                why = "hit the outer deadline" if rc == 124 else f"exit {rc}"
                print(f"{t}: the campaign did not finish ({why}):\n{out[-2000:]}", file=sys.stderr)
                return 2
        fresh = sorted(
            (p for n, p in artifacts(t).items() if n.startswith(KINDS) and (args.replay_only or n not in before)),
            key=lambda p: p.name,
        )
        runs.append({"target": t, "seconds": round(time.time() - started), "new_inputs": len(fresh)})
        for p in fresh:
            findings.append(investigate(args.toolchain, t, p, args.tmin_seconds))

    # One entry per distinct failure.
    seen, unique = set(), []
    for f in findings:
        # Only a replay that failed the way the finding did has a message to compare; every
        # other input stays its own entry.
        key = (f["target"], f["message"] or f["sha1"])
        if key not in seen:
            seen.add(key)
            unique.append(f)

    print("# Fuzz campaign\n")
    print("| target | seconds | new inputs |\n|---|---:|---:|")
    for r in runs:
        print(f"| {r['target']} | {r['seconds']} | {r['new_inputs']} |")
    print(f"\n{len(findings)} new input(s), {len(unique)} distinct failure(s).\n")
    for f in unique:
        print(f"## {f['target']}: {f['kind']} ({f['bytes']} bytes, sha1 {f['sha1'][:12]})")
        if f["message"]:
            print(f"- message: `{f['message']}`")
        if f["minimised"]:
            print(f"- minimised ({f['minimisation']}): `{f['minimised']}`")
        else:
            print(f"- NOT minimised ({f['minimisation']}); the input below is the original: `{f['artifact']}`")
        print(f"- reproduces on replay: {f['reproduces']} ({f['replay']})")
        if f["hex"] is not None:
            print(f"- hex: `{f['hex']}`")
        print()
    if args.json:
        Path(args.json).write_text(json.dumps({"runs": runs, "findings": unique}, indent=2), encoding="utf-8")
    return 1 if unique else 0


if __name__ == "__main__":
    sys.exit(main())
