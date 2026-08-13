#!/usr/bin/env python3
"""Build and attest the native arm64 Sylvander Harbor runner."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import subprocess
import tempfile
from pathlib import Path


def run(*command: str) -> str:
    return subprocess.run(command, check=True, text=True, capture_output=True).stdout.strip()


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--output-dir", type=Path, default=Path(
        "/private/tmp/sylvander-harbor-agent-linux-aarch64"
    ))
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[2]
    if run("git", "-C", str(root), "status", "--porcelain"):
        raise RuntimeError("refusing to build from a dirty worktree")
    commit = run("git", "-C", str(root), "rev-parse", "HEAD")
    machine = json.loads(run("podman", "machine", "inspect"))[0]
    if machine["State"] != "running" or machine.get("Rosetta") is not False:
        raise RuntimeError("Podman machine must be running with Rosetta disabled")
    if run("podman", "info", "--format", "{{.Host.Arch}}") != "arm64":
        raise RuntimeError("Podman server is not native arm64")

    args.output_dir.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(dir=args.output_dir) as staging_value:
        staging = Path(staging_value)
        subprocess.run([
            "podman", "run", "--rm", "--arch", "arm64",
            "-v", f"{root}:/workspace:ro",
            "-v", "sylvander-cargo-registry:/usr/local/cargo/registry",
            "-v", "sylvander-agent-target:/target",
            "-v", f"{staging}:/output",
            "-w", "/workspace", "-e", "CARGO_TARGET_DIR=/target",
            "docker.io/library/rust:1.96-alpine", "sh", "-c",
            "apk add --no-cache musl-dev openssl-dev openssl-libs-static pkgconf perl make >/dev/null "
            "&& cargo build --locked --release -p sylvander-benchmark-agent "
            "--bin sylvander-harbor-agent && cp /target/release/sylvander-harbor-agent /output/runner",
        ], check=True)
        runner = staging / "runner"
        digest = hashlib.sha256(runner.read_bytes()).hexdigest()
        binary = run("file", str(runner))
        if "ARM aarch64" not in binary or "statically linked" not in binary:
            raise RuntimeError(f"unexpected runner format: {binary}")
        run("podman", "run", "--rm", "--arch", "arm64", "-v",
            f"{runner}:/runner:ro", "alpine:3.22", "/runner", "--self-check")
        metadata = {
            "git_commit": commit,
            "sha256": digest,
            "architecture": "aarch64",
            "container_image": "docker.io/library/rust:1.96-alpine",
        }
        versioned = args.output_dir / f"sylvander-harbor-agent.{commit[:9]}"
        shutil.copy2(runner, versioned)
        Path(f"{versioned}.json").write_text(json.dumps(metadata, indent=2) + "\n")
        shutil.copy2(versioned, args.output_dir / "sylvander-harbor-agent.current")
        shutil.copy2(Path(f"{versioned}.json"), args.output_dir / "sylvander-harbor-agent.current.json")
    print(f"commit={commit} sha256={digest} runner={versioned}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
