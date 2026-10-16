"""Run test targets under Miri and report one result per test, on the host or on a cross target.

A test that hits undefined behavior, or an operation Miri cannot run, stops the whole Miri run, so
a plain `cargo miri test` names only the first problem of a binary. This runs a binary in one go;
when a test stops the run, that test is recorded and the run starts again without the tests that
are already accounted for. Per test the result is one of: pass, ignored, UB (undefined behavior),
panic, unsupported, deadlock, leak, timeout, notfound, error.

Both aliasing models run: Stacked Borrows (Miri's default, "SB") and Tree Borrows ("TB"). No Miri
flag is added unless `--flags` says so; in particular isolation stays on.

Reading a result:
- `unsupported` means Miri did not run that code. It is not a pass.
- A cross target (for example `x86_64-unknown-linux-gnu`) runs that target's dispatcher and OS
  calls. A pass there says nothing about the Windows dispatcher, real timers, or Windows FFI.
- `dyn QObject` is not `Unpin`, so Miri does not check the uniqueness of `&mut dyn QObject` or
  `Box<dyn QObject>`; a pass through them is no evidence. Test the same use on a concrete type.
- `--flags "-Zmiri-disable-isolation"` is for one test at a time that needs the environment. Say
  why, and check that it solves an environment limit and not an aliasing problem.

Usage (from the repository root; a nightly toolchain with the `miri` component):
    python tools/miri_sweep.py --crate ferrokit-core --targets lib,test_signal_owner_binding
        [--cross x86_64-unknown-linux-gnu] [--toolchain nightly] [--models SB,TB]
        [--timeout SECONDS] [--flags "..."] [--out results.json]

`--targets` is a comma list of integration test names, or `lib` for the unit tests. Names come
from `cargo test -- --list` on the default toolchain. The exit code is always 0.
"""

import argparse
import json
import os
import re
import subprocess
import sys
import time


def target_args(target):
    return ["--lib"] if target == "lib" else ["--test", target]


def list_tests(crate, target):
    p = subprocess.run(
        ["cargo", "test", "-p", crate] + target_args(target) + ["--", "--list", "--format", "terse"],
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


def run_miri(args, target, model, skip):
    env = dict(os.environ)
    flags = args.flags.split()
    if model == "TB":
        flags.append("-Zmiri-tree-borrows")
    if flags:
        env["MIRIFLAGS"] = " ".join(flags)
    else:
        env.pop("MIRIFLAGS", None)
    cmd = ["cargo", f"+{args.toolchain}", "miri", "test", "-p", args.crate]
    if args.cross:
        cmd += ["--target", args.cross]
    cmd += target_args(target) + ["--", "--test-threads=1"]
    if skip:
        cmd.append("--exact")
        for name in sorted(skip):
            cmd += ["--skip", name]
    try:
        p = subprocess.run(
            cmd, env=env, capture_output=True, text=True, timeout=args.timeout, encoding="utf-8", errors="replace"
        )
        return p.stdout + "\n" + p.stderr, False
    except subprocess.TimeoutExpired as e:
        parts = [(x.decode("utf-8", "replace") if isinstance(x, bytes) else x or "") for x in (e.stdout, e.stderr)]
        return "\n".join(parts), True


def failure_section(text, name):
    m = re.search(r"^---- " + re.escape(name) + r" stdout ----\n(.*?)(?=^---- |\nfailures:\n|\Z)", text, re.M | re.S)
    return m.group(1) if m else ""


def sweep_target(args, target, names, model, results, excerpts):
    done = set()
    started = time.time()
    for _ in range(len(names) + 3):
        if len(done) == len(names):
            break
        text, timed_out = run_miri(args, target, model, done)
        for name, outcome in re.findall(r"^test (\S+) \.\.\. (ok|FAILED|ignored)\s*$", text, re.M):
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
                results[key] = {"kind": kind, "sig": sig}
                excerpts[key] = section[:3000]
            else:
                results[key] = {"kind": "pass" if outcome == "ok" else "ignored", "sig": None}
            done.add(name)
        # A test that started and never reported is what stopped the run.
        started_names = re.findall(r"^test (\S+) \.\.\. (?!ok\s*$|FAILED\s*$|ignored\s*$)", text, re.M)
        stopper = next((n for n in reversed(started_names) if n in names and n not in done), None)
        if stopper:
            tail = text[text.rfind(f"test {stopper} ... "):]
            key = f"{target}::{stopper}::{model}"
            if timed_out:
                results[key] = {"kind": "timeout", "sig": None}
            else:
                kind, sig = classify(tail)
                results[key] = {"kind": kind, "sig": sig}
            excerpts[key] = tail[:3000]
            done.add(stopper)
            print(f"{target}::{stopper} {model} {results[key]['kind']}", flush=True)
            continue
        if re.search(r"^test result: ", text, re.M):
            break
        # Nothing reported and nothing started: the run itself failed (build error, no such test).
        kind, sig = classify(text)
        for name in names:
            if name not in done:
                key = f"{target}::{name}::{model}"
                results[key] = {"kind": "notfound" if "running 0 tests" in text else kind, "sig": sig}
                excerpts[key] = text[-2000:]
                done.add(name)
        break
    counts = {}
    for name in names:
        kind = results.get(f"{target}::{name}::{model}", {"kind": "missing"})["kind"]
        counts[kind] = counts.get(kind, 0) + 1
    print(f"{target} {model}: {len(names)} tests {counts} in {round(time.time() - started)}s", flush=True)


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--crate", required=True)
    ap.add_argument("--targets", required=True, help="comma list of integration test names, or `lib`")
    ap.add_argument("--cross", help="cross target triple, e.g. x86_64-unknown-linux-gnu")
    ap.add_argument("--toolchain", default="nightly")
    ap.add_argument("--models", default="SB,TB")
    ap.add_argument("--timeout", type=int, default=10800, help="seconds for one Miri run")
    ap.add_argument("--flags", default="", help="extra MIRIFLAGS (none by default)")
    ap.add_argument("--out", help="write the results as JSON")
    args = ap.parse_args()

    results, excerpts, tests = {}, {}, {}
    for target in args.targets.split(","):
        tests[target] = list_tests(args.crate, target)
        for model in args.models.split(","):
            sweep_target(args, target, tests[target], model, results, excerpts)
    if args.out:
        with open(args.out, "w", encoding="utf-8") as f:
            json.dump({"crate": args.crate, "cross": args.cross, "tests": tests, "results": results,
                       "excerpts": excerpts}, f, indent=1)
    return 0


if __name__ == "__main__":
    sys.exit(main())
