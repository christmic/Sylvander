# Security verification

This is the repeatable release gate for the local-first Sylvander deployment.
Run it from a clean checkout:

```sh
./scripts/security-verify.sh
```

The script fails on tracked high-confidence credential patterns, an unlocked
dependency graph, RustSec vulnerabilities, or any selected boundary regression.
It requires `cargo-audit`. Yanked-package lookup is intentionally disabled
because the configured registry mirror does not provide that API; vulnerability
matching still uses the freshly fetched RustSec advisory database.

## Threat model and verified boundaries

| Threat | Enforced boundary | Release evidence |
|---|---|---|
| malformed or shape-confused client frames | strict tagged protocol types, unknown-field rejection, bounded channel frames | deterministic deletion/replacement corpus proves parsing is total; invalid roots, tags, fields, and UTF-8 fail closed |
| workspace escape or command argument injection | canonical workspace resolver, non-shell file tools, validated Git paths | traversal, symlink, parent-path, and shell-argument regression tests |
| cross-user, cross-Agent, or cross-client disclosure | boundary-derived ownership and composite channel identity | User Profile, relationship memory, production memory, and live Unix client isolation tests |
| credential disclosure | typed secret references and redacted public views/presentation | tracked-file scan, credential round-trip redaction, and TUI header/URL/JWT/private-key redaction tests |
| undeleted learned data | privileged CAS-guarded chain deletion | complete supersession-chain deletion plus content-safe audit verification |
| vulnerable Rust dependency | locked graph checked against RustSec | `cargo audit --no-yanked` |

Authentication and authorization details remain normative in
[`boundary-authorization.md`](boundary-authorization.md). Container and sandbox
process restrictions are specified in
[`server-configuration.md`](server-configuration.md).

## Residual risk

- SSH execution is outside the current local release scope and has not passed
  the remote host-key, process-tree, or transfer threat model.
- The secret scan is a high-confidence deterministic gate, not a replacement
  for organization-wide history scanning or provider-side credential
  revocation.
- Yanked dependency status cannot be queried through the configured registry
  mirror. RustSec vulnerability advisories are still checked; release operators
  should restore a registry with yanked metadata if that signal is required.
- A real Docker/Podman daemon smoke run remains environment-dependent. Fake OCI
  contract tests prove argv, lifecycle, cleanup, and filesystem behavior, but
  do not certify a particular host daemon configuration.
