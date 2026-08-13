# Sylvander Work release trust boundary

> Status: unsigned cross-platform verification workflow implemented but not yet
> executed remotely; trusted publishing is blocked on owner-supplied identities
> and update infrastructure.
>
> Verified against Tauri 2 distribution, updater, macOS signing, and Windows
> signing documentation on 2026-08-13.

## What this document owns

This is the release SSOT for the Desktop host. It separates reproducible build
evidence from publisher identity, notarization, update authenticity, and
distribution. A locally generated `.app`, `.dmg`, installer, or update archive
is not by itself a trusted release.

## Implemented verification

`.github/workflows/desktop-host.yml` is configured to run the locked Web and
native host checks on current macOS, Ubuntu, and Windows runners. Actions are pinned to immutable
commits, workflow permissions are read-only, Node is fixed to 24, Rust is fixed
by `rust-toolchain.toml`, and npm/cargo consume their lockfiles. The matrix
can prove compilation, tests, strict Clippy, and Web production assembly only
after all three jobs pass; it does not sign, notarize, publish, or claim
installer runtime behavior. No remote run is currently recorded as evidence.

Local macOS CI-mode bundling uses `CI=true npm run build`. It currently proves
that Tauri can produce an unsigned `.app` and `.dmg`. Interactive Finder layout
is cosmetic and is not release evidence.

## Publisher-owned inputs

No secret belongs in Git, a Tauri config file, a build log, or a Desktop
diagnostic. A publishing environment must inject these through its protected
secret store:

- macOS Developer ID certificate/private key plus certificate password;
- App Store Connect API issuer, key id, and private-key file, or the documented
  Apple ID app-password alternative, for notarization and stapling;
- Windows certificate or managed signing-service identity and its exact
  `signCommand` integration;
- Linux package-signing identity when the chosen distribution channel requires
  one;
- Tauri updater private key and password for signing update artifacts.

The updater verification public key and HTTPS endpoint are public release
configuration, but they are still owner decisions. They must identify the
actual publishing authority and immutable release service. Placeholder domains,
generated throwaway keys, disabled TLS validation, and unsigned fallback are
forbidden.

## Fail-closed update gate

The updater is intentionally not compiled or advertised yet. Enabling it
requires all of the following in one reviewed change:

1. Commit the real updater public key and HTTPS endpoint configuration.
2. Pin the official updater plugin and keep its generic commands unavailable to
   the WebView; expose only product-specific check/download/install actions.
3. Sign every platform artifact and the updater manifest in protected CI.
4. Verify signature rejection, downgrade policy, offline behavior, interrupted
   download recovery, and user-visible restart consent.
5. Publish the authenticated metadata and artifacts before enabling the UI.

Until those conditions are evidenced, absence of an update button is the
correct secure behavior rather than an incomplete unsigned updater.

## Current blockers

The repository contains no publishing workflow, updater endpoint, or updater
public key. The audited macOS machine reports zero valid code-signing
identities. Consequently code signing, notarization, Windows signing, Linux
package signing, and automatic updates are not complete and must not be marked
green in release documentation.
