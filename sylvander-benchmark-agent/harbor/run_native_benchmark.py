#!/usr/bin/env python3
"""Run staged Sylvander benchmarks in a native arm64 Podman sandbox."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import subprocess
import tempfile
import time
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


def runner_revision(runner: Path, runner_sha: str) -> str:
    metadata_path = Path(f"{runner}.json")
    if not metadata_path.is_file():
        raise RuntimeError(f"runner metadata not found: {metadata_path}")
    metadata = json.loads(metadata_path.read_text())
    if metadata.get("sha256") != runner_sha or metadata.get("architecture") != "aarch64":
        raise RuntimeError("runner metadata does not match the selected binary")
    revision = metadata.get("git_commit")
    if not isinstance(revision, str) or not revision:
        raise RuntimeError("runner metadata has no git_commit")
    return revision


def trajectory_waterline(job_dir: Path) -> str | None:
    paths = list(job_dir.glob("*/agent/trajectory.json"))
    if not paths:
        return None
    trajectory = json.loads(paths[0].read_text())
    observable = trajectory.get("extra", {}).get("sylvander_observability", {})
    events = observable.get("events", [])
    last = events[-1].get("kind", "none") if events else "none"
    return (
        f"status={observable.get('status', 'unknown')} "
        f"steps={len(trajectory.get('steps', []))} events={len(events)} last={last}"
    )


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
    parser.add_argument("--model", default="MiniMax-M2.7")
    parser.add_argument("--harbor", type=Path, default=Path(
        "/private/tmp/sylvander-harbor-minimal/bin/harbor"
    ))
    args = parser.parse_args()

    task = (args.task or Path(DEFAULTS[args.level])).resolve()
    runner = args.runner.resolve()
    runner_sha = require_native_arm64(runner)
    revision = runner_revision(runner, runner_sha)
    image = require_native_task(task)
    if "/" in args.model or not args.model.strip():
        raise RuntimeError("--model must be a non-empty provider-local model ID")
    key = os.environ.get("SYLVANDER_BENCH_API_KEY", "")
    if args.level != "smoke" and not key:
        raise RuntimeError("set SYLVANDER_BENCH_API_KEY; it will not be placed in argv")

    model_slug = "".join(character.lower() if character.isalnum() else "-" for character in args.model)
    job_name = args.job_name or f"sylvander-{args.level}-{model_slug}-{revision[:9]}"
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
            "model_name": f"minimax-cn/{args.model}",
            "env": agent_env,
        }],
        "tasks": [{"path": str(task)}],
        "artifacts": ["/logs/agent/trajectory.json"],
    }
    command = [str(args.harbor), "run", "--config", "CONFIG", "--yes"]
    if args.level == "smoke":
        command.append("--install-only")

    print(f"level={args.level} model={args.model} commit={revision} runner_sha256={runner_sha}")
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
        process = subprocess.Popen(command, env=child_env)
        previous = None
        while process.poll() is None:
            time.sleep(15)
            waterline = trajectory_waterline(job_dir)
            if waterline and waterline != previous:
                print(f"waterline {waterline}", flush=True)
                previous = waterline
        completed_code = process.returncode
    leaked = redact_and_check(job_dir, key) if key and job_dir.exists() else 0
    if leaked:
        raise RuntimeError(f"redacted credential from {leaked} job artifact(s); run is invalid")
    return completed_code


if __name__ == "__main__":
    raise SystemExit(main())
