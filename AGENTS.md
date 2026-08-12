# Sylvander engineering rules

This file is the repository-local implementation policy. It applies to every
tracked source file unless a deeper `AGENTS.md` narrows a rule.

## Evidence before implementation

- Protocol implementations must be derived from the locally downloaded
  official SDK source and the corresponding official API documentation.
- Record the upstream repository, exact commit, inspected source paths, and
  mapped Sylvander files in the protocol design document.
- Do not infer wire fields, event names, error shapes, or token semantics from
  third-party compatible providers.
- Mock fixtures must use official response shapes. A fixture invented only to
  fit an implementation is not protocol evidence.

## Rust namespace imports

- Put every `use` declaration in the module import section, after module-level
  documentation and attributes and before type, constant, trait, `impl`, or
  function declarations.
- Do not place `use` declarations inside functions, methods, `impl` blocks,
  match arms, test bodies, or other nested scopes.
- Prefer a fully qualified path when moving an import to module scope would
  create an ambiguous or misleading short name.
- A local import is allowed only when a documented compiler, macro-expansion,
  or conditional-compilation constraint makes a module-level import
  impossible. Add an English `// Local import required: ...` comment at the
  exception site.
- New and modified Rust files must have zero undocumented local imports. The
  repository-wide local-import inventory is migration debt and must decrease,
  never increase, in each change.

## Verification

- Format, strict all-target Clippy, tests, and warning-denied Rustdoc must pass
  before completion is claimed.
- Run a source scan for indented `use` declarations. Every result must either
  predate the change and remain outside its files or carry the required
  exception comment; new LLM code must have no results.
- Real-provider tests remain ignored unless credentials and endpoints are
  supplied explicitly by the caller. LLM crates must never read environment
  variables for endpoint or authentication discovery.
