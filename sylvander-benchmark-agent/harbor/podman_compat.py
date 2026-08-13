#!/usr/bin/env python3
"""Expose Podman through the Docker CLI surface used by Harbor."""

from __future__ import annotations

import os
import sys
from collections.abc import Sequence


def translate_arguments(arguments: Sequence[str]) -> tuple[list[str], str | None]:
    """Return Podman arguments and the requested Compose working directory."""
    translated = list(arguments)
    if not translated or translated[0] != "compose":
        return translated, None

    compose_arguments: list[str] = []
    project_directory: str | None = None
    index = 1
    while index < len(translated):
        argument = translated[index]
        if argument == "--project-directory":
            if index + 1 >= len(translated):
                raise ValueError("--project-directory requires a path")
            if project_directory is not None:
                raise ValueError("--project-directory may be specified only once")
            project_directory = translated[index + 1]
            index += 2
            continue
        if argument.startswith("--project-directory="):
            if project_directory is not None:
                raise ValueError("--project-directory may be specified only once")
            project_directory = argument.partition("=")[2]
            if not project_directory:
                raise ValueError("--project-directory requires a path")
            index += 1
            continue
        compose_arguments.append(argument)
        index += 1

    return ["compose", *compose_arguments], project_directory


def main() -> int:
    try:
        arguments, project_directory = translate_arguments(sys.argv[1:])
    except ValueError as error:
        print(f"docker compatibility error: {error}", file=sys.stderr)
        return 2

    if project_directory is not None:
        os.chdir(project_directory)
    os.execvp("podman", ["podman", *arguments])
    return 127


if __name__ == "__main__":
    raise SystemExit(main())
