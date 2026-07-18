#!/bin/sh
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$repo_root"

python3 - <<'PY'
from __future__ import annotations

import os
import re
import subprocess
import sys
from pathlib import Path
from urllib.parse import unquote

root = Path.cwd()
index = root / "docs/INDEX.md"

# Every first-party Cargo package has exactly one indexed module-boundary
# document. Workspace-only manifests intentionally do not appear here.
boundaries = {
    "sylvander-agent/Cargo.toml": "sylvander-agent/docs/ARCHITECTURE.md",
    "sylvander-channel/Cargo.toml": "sylvander-channel/docs/ARCHITECTURE.md",
    "sylvander-channel-dingtalk/Cargo.toml": "docs/module-sylvander-channel-dingtalk.md",
    "sylvander-channel-http/Cargo.toml": "docs/module-sylvander-channel-http.md",
    "sylvander-channel-telegram/Cargo.toml": "docs/module-sylvander-channel-telegram.md",
    "sylvander-channel-unix/Cargo.toml": "docs/module-sylvander-channel-unix.md",
    "sylvander-channel-wechat/Cargo.toml": "docs/module-sylvander-channel-wechat.md",
    "sylvander-channel-ws/Cargo.toml": "docs/module-sylvander-channel-ws.md",
    "sylvander-llm-anthropic/Cargo.toml": "sylvander-llm-anthropic/docs/ARCHITECTURE.md",
    "sylvander-llm-core/Cargo.toml": "docs/module-sylvander-llm-core.md",
    "sylvander-protocol/Cargo.toml": "docs/module-sylvander-protocol.md",
    "sylvander-runtime/Cargo.toml": "sylvander-runtime/docs/ARCHITECTURE.md",
    "sylvander-server/Cargo.toml": "docs/module-sylvander-server.md",
    "sylvander-token9/token9-contracts/Cargo.toml":
        "sylvander-token9/token9-contracts/docs/ARCHITECTURE.md",
    "sylvander-token9/token9-server/Cargo.toml":
        "sylvander-token9/token9-server/docs/ARCHITECTURE.md",
    "sylvander-tui/Cargo.toml": "sylvander-tui/docs/ARCHITECTURE.md",
}

errors: list[str] = []
package_manifests: set[str] = set()
manifest_names = subprocess.run(
    [
        "git",
        "ls-files",
        "--cached",
        "--others",
        "--exclude-standard",
        "--",
        "*Cargo.toml",
    ],
    check=True,
    text=True,
    stdout=subprocess.PIPE,
).stdout.splitlines()
for relative in manifest_names:
    manifest = root / relative
    if re.search(r"(?m)^\[package\]\s*$", manifest.read_text(encoding="utf-8")):
        package_manifests.add(relative)

expected = set(boundaries)
for missing in sorted(package_manifests - expected):
    errors.append(f"first-party package has no boundary mapping: {missing}")
for stale in sorted(expected - package_manifests):
    errors.append(f"boundary mapping has no first-party package: {stale}")

index_text = index.read_text(encoding="utf-8")
link_pattern = re.compile(r"!?\[[^\]]*\]\(([^)\s]+)(?:\s+[^)]*)?\)")
index_targets = {match.group(1).strip("<>") for match in link_pattern.finditer(index_text)}
for manifest, document in sorted(boundaries.items()):
    path = root / document
    if not path.is_file():
        errors.append(f"{manifest}: missing boundary document {document}")
        continue
    target = os.path.relpath(path, index.parent).replace(os.sep, "/")
    if target not in index_targets:
        errors.append(f"{manifest}: docs/INDEX.md does not link {target}")

scan_roots = [
    root / "docs",
    root / "sylvander-agent/docs",
    root / "sylvander-channel/docs",
    root / "sylvander-llm-anthropic/docs",
    root / "sylvander-runtime/docs",
    root / "sylvander-tui/docs",
    root / "sylvander-token9/token9-contracts/docs",
    root / "sylvander-token9/token9-server/docs",
]
markdown_files: set[Path] = {
    root / "sylvander-runtime/GUARDIAN.md",
    root / "sylvander-runtime/CREDENTIAL_AUDIT.md",
    root / "sylvander-token9/README.md",
}
for scan_root in scan_roots:
    markdown_files.update(scan_root.rglob("*.md"))

reference_pattern = re.compile(r"(?m)^\s*\[[^\]]+\]:\s*(\S+)")
for document in sorted(markdown_files):
    text = document.read_text(encoding="utf-8")
    targets = [match.group(1) for match in link_pattern.finditer(text)]
    targets += [match.group(1) for match in reference_pattern.finditer(text)]
    for raw_target in targets:
        target = raw_target.strip("<>")
        if (
            not target
            or target.startswith("#")
            or re.match(r"^[a-zA-Z][a-zA-Z0-9+.-]*:", target)
        ):
            continue
        path_part = unquote(target.split("#", 1)[0].split("?", 1)[0])
        if not path_part:
            continue
        resolved = (document.parent / path_part).resolve()
        if not resolved.exists():
            errors.append(
                f"{document.relative_to(root)}: broken relative link {raw_target}"
            )

if errors:
    for error in errors:
        print(f"docs verification: {error}", file=sys.stderr)
    raise SystemExit(1)

print(
    "docs verification: "
    f"{len(boundaries)} crate boundaries indexed; "
    f"{len(markdown_files)} Markdown files checked"
)
PY
