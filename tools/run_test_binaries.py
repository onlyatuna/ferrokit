"""Run every ferrokit workspace test target on its own, with a time limit per target.

`cargo test` stops the whole run when one test binary hangs. This builds all test targets once
(`cargo test --workspace --no-run`), then starts each test executable directly, one at a time, with
the working directory and environment `cargo test` gives it. It kills the whole process tree when a
target runs past the limit, and reports per target: result, elapsed time, and on timeout the tail of
its output (libtest names the tests that have been running for over 60 seconds). Diagnostic only;
the exit code is 1 if any target failed or timed out. Doc tests are not included
(`cargo test --doc`).

Starting the executables directly saves cargo's start-up (manifest and lockfile load, fingerprint
check of the whole graph) on every one of the ~280 targets. What cargo does for a test run, and so
what `test_environment` reproduces (cargo/src/compiler/compilation.rs `fill_env`, `target_process`;
cargo/src/ops/cargo_test.rs `cmd_builds`; cargo/src/compiler/artifact.rs `get_env`):

- the working directory is the package's manifest directory;
- the dynamic-library search variable (PATH on Windows, LD_LIBRARY_PATH on Linux,
  DYLD_FALLBACK_LIBRARY_PATH on macOS) gets the profile directory, `deps/` and the toolchain's
  target libdir in front of its inherited value;
- CARGO (the cargo executable), CARGO_MANIFEST_DIR, CARGO_MANIFEST_PATH, CARGO_PKG_NAME,
  CARGO_PKG_VERSION and its _MAJOR/_MINOR/_PATCH/_PRE parts, and CARGO_PKG_DESCRIPTION, _HOMEPAGE,
  _REPOSITORY, _LICENSE, _LICENSE_FILE, _AUTHORS (joined with ':'), _RUST_VERSION, _README;
- CARGO_BIN_EXE_<name> for each binary target of the package, for integration tests and benches;
- the `[env]` table of `.cargo/config.toml` (a key that is already set is kept unless `force`).

Not reproduced because the workspace has no use for it: variables from build scripts of the package
itself (`cargo::rustc-env`; no member has a build script) and artifact-dependency variables
(unstable). CARGO_CRATE_NAME, CARGO_TARGET_TMPDIR and CARGO_PRIMARY_PACKAGE are compile-time
variables of the rustc process (`env!`), not set in a running test. The variables the rustup proxy
adds to the cargo it starts (RUSTUP_HOME, RUSTUP_TOOLCHAIN, RUSTUP_TOOLCHAIN_SOURCE, CARGO_HOME,
RUST_RECURSION_COUNT, the toolchain `lib` directory on the library path, the toolchain `bin`
directory on PATH) reach a test through cargo and are not set here, nor are the SSL_CERT_FILE and
SSL_CERT_DIR that cargo sets for itself on Linux (openssl-probe); no test reads them. Diffing the
environment of a throwaway test under `cargo test` and under this launcher on Windows, Linux and
macOS runners shows no other difference.

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
import tomllib


def norm(path):
    return os.path.normcase(os.path.normpath(path))


def parse_test_targets(lines, packages_by_manifest, packages=None):
    """Map `cargo test --no-run --message-format=json` lines to test targets.

    `packages_by_manifest` maps a normalized manifest path to its `cargo metadata` package.
    Returns dicts with the display `name`, the `executable`, the metadata `package`, and `is_test`
    (integration test or bench: cargo sets CARGO_BIN_EXE_* for these only).
    """
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
        package = packages_by_manifest[norm(msg["manifest_path"])]
        if packages and package["name"] not in packages:
            continue
        found.append(
            {
                "name": "{} {}".format(package["name"], msg["target"]["name"]),
                "executable": msg["executable"],
                "package": package,
                "is_test": msg["target"]["kind"][0] in ("test", "bench"),
            }
        )
    return found


def dylib_path_var():
    if os.name == "nt":
        return "PATH"
    return "DYLD_FALLBACK_LIBRARY_PATH" if sys.platform == "darwin" else "LD_LIBRARY_PATH"


def config_env(cwd, environ, cargo_home):
    """The `[env]` entries cargo applies: nearest `.cargo/config.toml` first, then `$CARGO_HOME`."""
    dirs = []
    cur = os.path.abspath(cwd)
    while True:
        dirs.append(os.path.join(cur, ".cargo"))
        parent = os.path.dirname(cur)
        if parent == cur:
            break
        cur = parent
    dirs.append(cargo_home)
    result = {}
    for d in dirs:
        for name in ("config.toml", "config"):
            path = os.path.join(d, name)
            if os.path.isfile(path):
                break
        else:
            continue
        with open(path, "rb") as f:
            table = tomllib.load(f).get("env", {})
        for key, value in table.items():
            if key in result:
                continue
            if isinstance(value, dict):
                force, relative, value = value.get("force", False), value.get("relative", False), value["value"]
            else:
                force, relative = False, False
            if relative:
                value = os.path.join(os.path.dirname(d), value)
            result[key] = (value, force)
    return {key: value for key, (value, force) in result.items() if force or key not in environ}


def test_environment(target, environ, tool):
    """The environment `cargo test` runs `target` with; `tool` describes the toolchain and workspace."""
    package = target["package"]
    env = dict(environ)
    exe_dir = os.path.dirname(target["executable"])
    search = [os.path.dirname(exe_dir), exe_dir, tool["sysroot_libdir"]]
    var = dylib_path_var()
    existing = [p for p in environ.get(var, "").split(os.pathsep) if p]
    if existing[: len(search)] != search:
        existing = search + existing
    if sys.platform == "darwin" and not environ.get(var):
        # Cargo keeps the dyld defaults alive when it sets the variable.
        home = environ.get("HOME")
        existing += ([os.path.join(home, "lib")] if home else []) + ["/usr/local/lib", "/usr/lib"]
    env[var] = os.pathsep.join(existing)

    manifest = package["manifest_path"]
    version = package["version"]
    core, _, rest = version.partition("-")
    major, minor, patch = (core.split("+")[0].split(".") + ["0", "0", "0"])[:3]
    pre = rest.split("+")[0]
    cargo = {
        "CARGO": tool["cargo"],
        "CARGO_MANIFEST_DIR": os.path.dirname(manifest),
        "CARGO_MANIFEST_PATH": manifest,
        "CARGO_PKG_VERSION_MAJOR": major,
        "CARGO_PKG_VERSION_MINOR": minor,
        "CARGO_PKG_VERSION_PATCH": patch,
        "CARGO_PKG_VERSION_PRE": pre,
        "CARGO_PKG_VERSION": version,
        "CARGO_PKG_NAME": package["name"],
        "CARGO_PKG_DESCRIPTION": package.get("description") or "",
        "CARGO_PKG_HOMEPAGE": package.get("homepage") or "",
        "CARGO_PKG_REPOSITORY": package.get("repository") or "",
        "CARGO_PKG_LICENSE": package.get("license") or "",
        "CARGO_PKG_LICENSE_FILE": package.get("license_file") or "",
        "CARGO_PKG_AUTHORS": ":".join(package.get("authors") or []),
        "CARGO_PKG_RUST_VERSION": package.get("rust_version") or "",
        "CARGO_PKG_README": package.get("readme") or "",
    }
    if target["is_test"]:
        suffix = ".exe" if os.name == "nt" else ""
        for t in package["targets"]:
            if "bin" in t["kind"]:
                cargo["CARGO_BIN_EXE_" + t["name"]] = os.path.join(os.path.dirname(exe_dir), t["name"] + suffix)
    env.update(cargo)
    for key, value in config_env(os.path.dirname(manifest), environ, tool["cargo_home"]).items():
        # Cargo never overrides a value it set itself.
        if key not in cargo and key != var:
            env[key] = value
    return env


def test_command(target, test_threads=None):
    cmd = [target["executable"]]
    if test_threads:
        cmd += ["--test-threads", test_threads]
    return cmd


def run_text(cmd, **kwargs):
    return subprocess.run(cmd, stdout=subprocess.PIPE, check=True, **kwargs).stdout.decode("utf-8", "replace")


def toolchain():
    """The cargo executable, the target libdir of the toolchain and the cargo home."""
    sysroot = run_text(["rustc", "--print", "sysroot"]).strip()
    host = next(l.split(":", 1)[1].strip() for l in run_text(["rustc", "-vV"]).splitlines() if l.startswith("host:"))
    cargo = os.path.join(sysroot, "bin", "cargo" + (".exe" if os.name == "nt" else ""))
    return {
        "cargo": cargo if os.path.isfile(cargo) else "cargo",
        "sysroot_libdir": os.path.join(sysroot, "lib", "rustlib", host, "lib"),
        "cargo_home": os.environ.get("CARGO_HOME") or os.path.join(os.path.expanduser("~"), ".cargo"),
    }


def test_targets(jobs, packages=None):
    meta = json.loads(run_text(["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"]))
    by_manifest = {norm(p["manifest_path"]): p for p in meta["packages"]}
    out = run_text(["cargo", "test", "--locked", "-j", jobs, "--workspace", "--no-run", "--message-format=json"])
    return parse_test_targets(out.splitlines(), by_manifest, packages)


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
    build_start = time.monotonic()
    targets = test_targets(args.jobs, args.package)
    build_seconds = time.monotonic() - build_start
    if not targets:
        print("error: no test targets found; refusing to report success", file=sys.stderr)
        return 1
    tool = toolchain()
    for target in targets:
        name = target["name"]
        print("::group::{}".format(name), flush=True)
        start = time.monotonic()
        proc = subprocess.Popen(
            test_command(target, args.test_threads),
            cwd=os.path.dirname(target["package"]["manifest_path"]),
            env=test_environment(target, os.environ, tool),
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
    print("build phase {:.0f}s, run phase {:.0f}s".format(build_seconds, sum(r[2] for r in results)))
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
