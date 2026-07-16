# Skill packages

Sylvander loads Skill packages from the Agent home first and the task
workspace second. Later task-workspace content therefore has higher prompt
precedence. Each workspace is searched, in order, under:

1. `.agents/skills/`
2. `.sylvander/skills/`
3. `skills/`

A package is one directory containing a non-empty UTF-8 `SKILL.md`. It may
also contain `SKILL.toml`:

```toml
schema_version = 1
name = "careful-review"
enabled = true
resources = [
  "references/checklist.md",
  "templates/report.md",
]
```

`SKILL.toml` is strict: unknown fields, unsupported schema versions, invalid
names, duplicate resources, absolute paths, parent traversal, or more than 16
resources invalidate the whole package. Names use at most 64 ASCII letters,
digits, `-`, or `_`.

Packages without a manifest remain active and use the directory name. A
manifest with `enabled = false` keeps the package visible but does not load
its content. Declared resources are exact paths relative to the package;
globbing and implicit directory loading are intentionally unsupported.

## Activation and isolation

The runtime reads `SKILL.md` followed by declared resources on every turn
through the selected workspace executor. Every document is limited to 16 KiB;
all workspace instructions and Skills share a 48 KiB, 24-document budget.
A missing, empty, non-UTF-8, truncated, or over-budget document degrades the
entire package. Partial package content is never added to the model prompt.

The platform snapshot exposes each discovered package with:

- `active` for an injected package;
- `configured` for a disabled package;
- `degraded` for an invalid or unavailable package;
- Agent-home packages as built-in trust and task packages as workspace trust;
- content-free health summaries, source provenance, capabilities, and
  per-turn reload truth.

The snapshot never contains Skill instructions or resource contents. UI
clients inspect it through the normal platform status protocol rather than
reading workspace files directly.
