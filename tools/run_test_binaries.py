"""Run every ferrokit workspace test target on its own, with a time limit per target.

`cargo test` stops the whole run when one test binary hangs. This builds all test targets once,
then runs `cargo test` for one target at a time (so the environment is exactly cargo's), kills the
whole process tree when a target runs past the limit, and reports per target: result, elapsed
time, and on timeout the tail of its output (libtest names the tests that have been running for
over 60 seconds). Diagnostic only; the exit code is 1 if any target failed or timed out. Doc tests
are not included (`cargo test --doc`).

Usage (from the repository root):
    python tools/run_test_binaries.py [--timeout SECONDS] [--jobs N] [--package NAME]... [--test-threads N]

`--package` (repeatable) runs only those packages' targets; `--test-threads` is passed to libtest.

`--jobs` is cargo's build parallelism (default 1); tests always run one target at a time.
"""

import argparse
import json
import os
import signal
import subprocess
import sys
import time

KIND_FLAGS = {"lib": "--lib", "test": "--test", "bin": "--bin", "example": "--example", "bench": "--bench"}


def parse_test_targets(lines, jobs, packages=None, test_threads=None):
    """Map `cargo test --no-run --message-format=json` lines to (name, command) pairs."""
    found = []
    for line in lines:
        try:
            msg = json.loads(line)
        except ValueError:
            continue
        if msg.get("reason") != "compiler-artifact" or not msg.get("executable"):
            continue
        if not msg.get("profile", {}).get("test"):
            continue
        kind = msg["target"]["kind"][0]
        flag = KIND_FLAGS.get(kind, "--lib")
        if kind in ("lib", "rlib", "proc-macro", "cdylib", "dylib", "staticlib"):
            flag = "--lib"
        package = os.path.basename(os.path.dirname(msg["manifest_path"]))
        if packages and package not in packages:
            continue
        cmd = ["cargo", "test", "--locked", "-j", jobs, "-p", package, flag]
        if flag != "--lib":
            cmd.append(msg["target"]["name"])
        if test_threads:
            cmd += ["--", "--test-threads", test_threads]
        found.append(("{} {}".format(package, msg["target"]["name"]), cmd))
    return found


def test_targets(jobs, packages=None, test_threads=None):
    out = subprocess.run(
        ["cargo", "test", "--locked", "-j", jobs, "--workspace", "--no-run", "--message-format=json"],
        stdout=subprocess.PIPE,
        check=True,
    ).stdout.decode("utf-8", "replace")
    return parse_test_targets(out.splitlines(), jobs, packages, test_threads)


def kill_tree(proc):
    if os.name == "nt":
        subprocess.run(["taskkill", "/F", "/T", "/PID", str(proc.pid)], capture_output=True)
    else:
        os.killpg(proc.pid, signal.SIGKILL)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--timeout", type=float, default=120.0, help="seconds per test target")
    parser.add_argument("--jobs", default="1", help="cargo build jobs (-j)")
    parser.add_argument("--package", action="append", help="only run this package's targets (repeatable)")
    parser.add_argument("--test-threads", help="libtest --test-threads value")
    args = parser.parse_args()

    results = []
    targets = test_targets(args.jobs, args.package, args.test_threads)
    if not targets:
        print("error: no test targets found; refusing to report success", file=sys.stderr)
        return 1
    for name, cmd in targets:
        print("::group::{}".format(name), flush=True)
        start = time.monotonic()
        proc = subprocess.Popen(
            cmd,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            start_new_session=(os.name != "nt"),
        )
        try:
            output, _ = proc.communicate(timeout=args.timeout)
            status = "ok" if proc.returncode == 0 else "FAILED"
        except subprocess.TimeoutExpired:
            kill_tree(proc)
            output, _ = proc.communicate()
            status = "TIMEOUT"
        elapsed = time.monotonic() - start
        text = output.decode("utf-8", "replace")
        print(text, flush=True)
        print("::endgroup::", flush=True)
        if status == "TIMEOUT":
            print("{}: TIMEOUT after {:.0f}s; last output:".format(name, elapsed))
            print("\n".join(text.splitlines()[-40:]), flush=True)
        results.append((name, status, elapsed))

    print("\n=== per-target summary ===")
    for name, status, elapsed in results:
        print("{:8} {:7.1f}s  {}".format(status, elapsed, name))
    bad = [r for r in results if r[1] != "ok"]
    print("{} targets, {} not ok".format(len(results), len(bad)))
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
