#!/usr/bin/env python3
"""Run staged Sylvander benchmarks in a native arm64 Podman sandbox."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import subprocess
import tempfile
from pathlib import Path


DEFAULTS = {
    "smoke": "/private/tmp/sylvander-native-arm64-tasks/cancel-async-tasks",
    "small": "/private/tmp/sylvander-native-arm64-tasks/cancel-async-tasks",
    "medium": "/private/tmp/sylvander-terminal-bench-2-native/sanitize-git-repo",
    "large": "/private/tmp/sylvander-terminal-bench-2-native/large-scale-text-editing",
}


def run(*command: str) -> str:
    result = subprocess.run(command, check=True, text=True, capture_output=True)
    return result.stdout.strip()


def require_native_arm64(runner: Path) -> str:
    machine = json.loads(run("podman", "machine", "inspect"))[0]
    if machine["State"] != "running" or machine.get("Rosetta") is not False:
        raise RuntimeError("Podman machine must be running with Rosetta disabled")
    if run("podman", "info", "--format", "{{.Host.Arch}}") != "arm64":
        raise RuntimeError("Podman server is not native arm64")
    binary = run("file", str(runner))
    if "ARM aarch64" not in binary or "statically linked" not in binary:
        raise RuntimeError(f"runner is not static aarch64: {binary}")
    run(
        "podman", "run", "--rm", "--arch", "arm64", "-v",
        f"{runner}:/runner:ro", "alpine:3.22", "/runner", "--self-check",
    )
    return hashlib.sha256(runner.read_bytes()).hexdigest()


def require_native_task(task: Path) -> str:
    task_config = task / "task.toml"
    if not task_config.is_file():
        raise RuntimeError(f"task.toml not found: {task_config}")
    image = next(
        (line.split("=", 1)[1].strip().strip('"') for line in task_config.read_text().splitlines()
         if line.strip().startswith("docker_image =")),
        None,
    )
    if not image:
        raise RuntimeError("task must pin docker_image")
    architecture = run("podman", "image", "inspect", image, "--format", "{{.Architecture}}")
    if architecture != "arm64":
        raise RuntimeError(f"task image is not native arm64: {architecture}")
    return image


def redact_and_check(job_dir: Path, secret: str) -> int:
    hits = 0
    encoded = secret.encode()
    for path in job_dir.rglob("*"):
        if not path.is_file():
            continue
        try:
            content = path.read_bytes()
        except OSError:
            continue
        if encoded not in content:
            continue
        hits += 1
        path.write_bytes(content.replace(encoded, b"[REDACTED]"))
    return hits


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("level", choices=DEFAULTS)
    parser.add_argument("--task", type=Path)
    parser.add_argument("--runner", type=Path, default=Path(
        "/private/tmp/sylvander-harbor-agent-linux-aarch64/sylvander-harbor-agent.current"
    ))
    parser.add_argument("--jobs-dir", type=Path, default=Path(
        "/private/tmp/sylvander-harbor-jobs"
    ))
    parser.add_argument("--job-name")
    parser.add_argument("--harbor", type=Path, default=Path(
        "/private/tmp/sylvander-harbor-minimal/bin/harbor"
    ))
    args = parser.parse_args()

    task = (args.task or Path(DEFAULTS[args.level])).resolve()
    runner = args.runner.resolve()
    runner_sha = require_native_arm64(runner)
    image = require_native_task(task)
    key = os.environ.get("SYLVANDER_BENCH_API_KEY", "")
    if args.level != "smoke" and not key:
        raise RuntimeError("set SYLVANDER_BENCH_API_KEY; it will not be placed in argv")

    revision = run("git", "rev-parse", "--short=9", "HEAD")
    job_name = args.job_name or f"sylvander-{args.level}-{revision}"
    job_dir = args.jobs_dir / job_name
    if job_dir.exists():
        raise RuntimeError(f"refusing to reuse job directory: {job_dir}")
    agent_env = {
        "SYLVANDER_HARBOR_BASE_URL": "https://api.minimaxi.com/v1",
        "SYLVANDER_HARBOR_PROTOCOL": "openai_chat_completions",
        "SYLVANDER_HARBOR_REQUIRED_ARCH": "aarch64",
        "SYLVANDER_HARBOR_BINARY_HOST_PATH": str(runner),
    }
    if key:
        agent_env["SYLVANDER_HARBOR_API_KEY"] = key
    config = {
        "job_name": job_name,
        "jobs_dir": str(args.jobs_dir),
        "n_concurrent_trials": 1,
        "environment": {"type": "docker", "delete": False},
        "agents": [{
            "name": "sylvander_agent:SylvanderAgent",
            "model_name": "minimax-cn/MiniMax-M2.7",
            "env": agent_env,
        }],
        "tasks": [{"path": str(task)}],
        "artifacts": ["/logs/agent/trajectory.json"],
    }
    command = [str(args.harbor), "run", "--config", "CONFIG", "--yes"]
    if args.level == "smoke":
        command.append("--install-only")

    print(f"level={args.level} commit={revision} runner_sha256={runner_sha}")
    print(f"task={task} image={image} job={job_dir}")
    with tempfile.NamedTemporaryFile("w", suffix=".json", delete=True) as config_file:
        os.chmod(config_file.name, 0o600)
        json.dump(config, config_file)
        config_file.flush()
        command[3] = config_file.name
        child_env = dict(os.environ)
        child_env.pop("SYLVANDER_BENCH_API_KEY", None)
        child_env.update({
            "PATH": f"{Path(__file__).parent}:{args.harbor.parent}:"
                    "/Users/christmix/.local/bin:/opt/homebrew/bin:/usr/bin:/bin",
            "PYTHONPATH": str(Path(__file__).parent),
        })
        completed = subprocess.run(command, env=child_env)
    leaked = redact_and_check(job_dir, key) if key and job_dir.exists() else 0
    if leaked:
        raise RuntimeError(f"redacted credential from {leaked} job artifact(s); run is invalid")
    return completed.returncode


if __name__ == "__main__":
    raise SystemExit(main())
