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
            return re.sub(r"0x[0-9a-f]+", "0x?", line.strip())[:300]
    tail = [l for l in output.splitlines() if l.strip()]
    return (tail[-1] if tail else "no output")[:300]


def minimise(toolchain, target, path, tmin_seconds):
    before = set(artifacts(target))
    cargo(toolchain, "tmin", target, str(path), "--", f"-max_total_time={tmin_seconds}", timeout=tmin_seconds + 120)
    new = [n for n in set(artifacts(target)) - before if n.startswith("minimized-from-")]
    return artifacts(target)[new[0]] if new else path


def replay(toolchain, target, path):
    rc, out = cargo(toolchain, "run", target, str(path), "--", "-runs=1", "-timeout=10", timeout=300)
    return rc, out


def investigate(toolchain, target, path, tmin_seconds):
    small = minimise(toolchain, target, path, tmin_seconds)
    rc, out = replay(toolchain, target, small)
    data = small.read_bytes()
    return {
        "target": target,
        "kind": next((k.rstrip("-") for k in KINDS if path.name.startswith(k)), "other"),
        "artifact": str(path.relative_to(ROOT)),
        "minimised": str(small.relative_to(ROOT)),
        "bytes": len(data),
        "sha1": hashlib.sha1(data).hexdigest(),
        "hex": data.hex() if len(data) <= HEX_LIMIT else None,
        "reproduces": rc != 0,
        "message": failure_message(out),
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
            rc, out = cargo(args.toolchain, "run", t, str(corpus), *extra, "--", *lf, timeout=args.seconds + 900)
            # A harness that does not build, or a missing toolchain, must not look like "no findings".
            built = "INFO:" in out  # libFuzzer started
            if rc not in (0, 124) and not built:
                print(f"{t}: the campaign could not run (exit {rc}):\n{out[-2000:]}", file=sys.stderr)
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
        key = (f["target"], f["message"])
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
        print(f"- message: `{f['message']}`")
        print(f"- minimised: `{f['minimised']}`  (reproduces on replay: {f['reproduces']})")
        if f["hex"] is not None:
            print(f"- hex: `{f['hex']}`")
        print()
    if args.json:
        Path(args.json).write_text(json.dumps({"runs": runs, "findings": unique}, indent=2), encoding="utf-8")
    return 1 if unique else 0


if __name__ == "__main__":
    sys.exit(main())
