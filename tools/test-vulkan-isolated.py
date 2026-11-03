#!/usr/bin/env python3
"""Run Rosemary's Metal Vulkan library tests with one SDL owner per process.

Usage:
  python3 tools/test-vulkan-isolated.py --mode default
  python3 tools/test-vulkan-isolated.py --mode fifo

A single libtest process is not safe across Rust test threads: SDL video
initialization is bound to a pthread, and --test-threads=1 does not guarantee
that every test is run on the same OS thread.
"""

import argparse
import json
import os
import pathlib
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
PREFLIGHT = "swapchain::tests::vulkan_graphical_test_prerequisites_are_available"


def compile_metal():
    command = [
        "cargo", "+1.99.0", "test", "--offline", "-p", "metal", "--lib",
        "--no-run", "--message-format=json",
    ]
    result = subprocess.run(command, cwd=ROOT, text=True, capture_output=True)
    if result.returncode:
        sys.stderr.write(result.stderr)
        raise RuntimeError("Metal library test compilation failed")
    bins = []
    for line in result.stdout.splitlines():
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            continue
        if (
            event.get("reason") == "compiler-artifact"
            and event.get("target", {}).get("name") == "metal"
            and "lib" in event.get("target", {}).get("kind", [])
            and event.get("executable")
        ):
            bins.append(pathlib.Path(event["executable"]))
    if len(bins) != 1:
        raise RuntimeError(f"expected exactly one Metal libtest binary, found {bins}")
    return bins[0]


def list_tests(binary, ignored_only=False):
    args = [str(binary), "--list"]
    if ignored_only:
        args.append("--ignored")
    result = subprocess.run(args, cwd=ROOT, text=True, capture_output=True, check=True)
    return sorted(
        line.removesuffix(": test").strip()
        for line in result.stdout.splitlines()
        if line.endswith(": test")
    )


def run_test(binary, name, env, timeout):
    try:
        result = subprocess.run(
            [str(binary), "--exact", name, "--test-threads=1"],
            cwd=ROOT, env=env, text=True, capture_output=True, timeout=timeout,
        )
        if result.returncode == 0 and "1 passed" in result.stdout:
            return None
        return f"{result.stdout}\n{result.stderr}\nexit={result.returncode}"
    except subprocess.TimeoutExpired as error:
        return f"timed out after {error.timeout} seconds"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--mode", choices=["default", "fifo"], default="default")
    parser.add_argument("--filter", default="", help="Test-name substring filter")
    parser.add_argument("--timeout", type=int, default=45, help="Seconds per test")
    args = parser.parse_args()

    if not os.getenv("DISPLAY") and not os.getenv("WAYLAND_DISPLAY"):
        parser.error("Vulkan WSI test requires an SDL display (DISPLAY or WAYLAND_DISPLAY)")
    env = os.environ.copy()
    env["ROSEMARY_TEST_VULKAN"] = "1"
    if args.mode == "fifo":
        env["ROSEMARY_METAL_PRESENTATION_MODE"] = "fifo"
    else:
        env.pop("ROSEMARY_METAL_PRESENTATION_MODE", None)

    binary = compile_metal()
    tests = list_tests(binary)
    ignored = set(list_tests(binary, ignored_only=True))
    if PREFLIGHT not in tests:
        raise RuntimeError("missing mandatory SDL video/Vulkan preflight regression")

    # Must run a *real* Vulkan+SDL surface setup before any test that may
    # otherwise silently return when adapters or presentation are unavailable.
    error = run_test(binary, PREFLIGHT, env, args.timeout)
    if error:
        raise RuntimeError(f"Vulkan graphical prerequisites failed:\n{error}")
    if args.mode == "fifo":
        tests = [
            name for name in tests
            if "fifo" in name or name == PREFLIGHT
        ]
    if args.filter:
        tests = [name for name in tests if args.filter in name]
    ignored_count = sum(name in ignored for name in tests)
    tests = [name for name in tests if name not in ignored and name != PREFLIGHT]
    print(
        f"Vulkan SDL-owner isolated suite: mode={args.mode} "
        f"binary={binary} selected={len(tests)} ignored={ignored_count}",
        flush=True,
    )
    failures = []
    for i, name in enumerate(tests, 1):
        error = run_test(binary, name, env, args.timeout)
        if error:
            failures.append((name, error))
            print(f"FAIL {i}/{len(tests)} {name}", flush=True)
        elif i % 10 == 0 or i == len(tests):
            print(f"PASS {i}/{len(tests)} (isolated processes)", flush=True)
    for name, details in failures:
        print(f"\nFAILED: {name}\n{details}", file=sys.stderr)
    print(
        f"RESULT: {len(tests)-len(failures)} passed, "
        f"{len(failures)} failed, {ignored_count} ignored; "
        f"preflight passed; mode={args.mode}",
        flush=True,
    )
    return bool(failures)


if __name__ == "__main__":
    sys.exit(main())
