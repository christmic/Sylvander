# sylvander-ghostty — upstream sync

This directory is a **git subtree** mirror of
[`ghostty-org/ghostty`](https://github.com/ghostty-org/ghostty),
embedded inside the `Sylvander` repository. It is **not** a fork on
GitHub — there is no `origin`, no fork repository, and we **never**
push back upstream.

## Why subtree (not submodule)

- Single repository, single `git log`, single `git blame`.
- We can edit files here and `git commit` lands on Sylvander's main
  branch — no separate push step.
- Ghostty's full history (16k+ commits) is preserved in Sylvander's
  history. `git log sylvander-ghostty/` and `git log --follow` work
  normally.

## How it was added

```sh
git subtree add --prefix=sylvander-ghostty \
    https://github.com/ghostty-org/ghostty.git main
```

No `--squash` — full history is retained.

## Routine: pulling upstream changes

When ghostty-org publishes new commits, sync them in:

```sh
# from the Sylvander repo root
cd ..

# Pull latest upstream. The first time git remembers the URL; subsequent
# pulls can omit it.
git subtree pull --prefix=sylvander-ghostty \
    https://github.com/ghostty-org/ghostty.git main
```

If you have local edits inside `sylvander-ghostty/`, the pull will
attempt to merge them with upstream. Resolve conflicts normally, then
commit. If the conflicts are nasty, fall back to:

```sh
# 1. See what changed upstream
git fetch https://github.com/ghostty-org/ghostty.git main
git log FETCH_HEAD -- sylvander-ghostty/ | head

# 2. Manual merge strategy if subtree merge is stuck
git checkout FETCH_HEAD -- sylvander-ghostty/
# then re-apply Sylvander patches on top
```

## Where our changes live

Sylvander-specific work goes here:

```
sylvander-ghostty/src/sylvander/   ← our Zig module (F1 → F6)
```

Upstream Ghostty files are **read-only by convention**. Edit them
only when:

1. The change is genuinely a bugfix that should live upstream too, OR
2. The change is required to integrate `src/sylvander/` and the
   alternative (keeping our code behind runtime checks) is worse.

When you do edit upstream files, gate the change behind
`config.sylvander.enabled` so non-Sylvander builds remain unaffected.

## Network / proxy

GitHub access requires a proxy on this network. Configure once:

```sh
git config --global http.version HTTP/1.1
git config --global http.proxy http://127.0.0.1:1081
```

HTTP/1.1 is forced because the proxy mishandles HTTP/2 framing.

## Building locally

The upstream Ghostty project pins Zig 0.15.2 and requires a specific
build environment. See `HACKING.md` for upstream's full document;
the Sylvander-specific notes below are what we actually use on macOS.

### Toolchain

| Tool | Version | Where to get it |
|---|---|---|
| Zig | **0.15.2** (homebrew-patched) | `brew install zig@0.15 && brew link --overwrite zig@0.15` |
| Xcode (macOS app build) | **Xcode 26** + macOS 26 SDK | App Store; not needed for `zig build` of the library / CLI |
| Rust (cargo) | stable | `rustup` |

### Why not zigup / Zig 0.16

- **Zig 0.16** has API breaks that Ghostty upstream's `build.zig` and
  std calls (e.g. `readFileAlloc`) haven't been updated for yet. Don't
  upgrade until upstream does.
- **zigup**-managed `zig` will give you vanilla upstream 0.15.2
  binaries, but on macOS with **Xcode 26.4** those have a known linking
  bug ([ziglang/zig#31658](https://codeberg.org/ziglang/zig/issues/31658)).
  The homebrew `zig@0.15` formula contains a backport patch that
  works around it. Use `brew install zig@0.15` for local development.

### Quick start

```bash
# One-time setup
brew install zig@0.15
brew link --overwrite zig@0.15
zig version    # expect 0.15.x

# Compile — same flags the CI `zig-checked` job uses. Without these,
# `zig build` defaults to emit-xcframework=true on macOS, which
# requires the macOS 26 SDK + triggers the findNative() calls in
# SharedDeps that fail without a full Xcode install.
cd Sylvander
cargo build --workspace
(cd sylvander-ghostty && zig build \
  -Dapp-runtime=none -Demit-xcframework=false -Demit-macos-app=false)

# Run the Sylvander module tests in isolation (no Xcode needed)
(cd sylvander-ghostty/src/sylvander && zig build test)

# Full upstream-parity test build (needs Xcode 26 SDK)
(cd sylvander-ghostty && zig build -Dapp-runtime=none test)
```

### If you're on Xcode 26.4 specifically

The `zig build` will fail with errors like:

```
error: undefined symbol: _realpath$DARWIN_EXTSN
error: undefined symbol: _sigaction
error: undefined symbol: _dispatch_queue_create
```

This is the known linking issue. Two options:

1. **Use the patched `brew install zig@0.15`** (preferred).
2. Downgrade to **Xcode 26.3** (workaround documented in
   `HACKING.md`).

### What we do in CI

GitHub Actions uses `mlugg/setup-zig@v2` to install Zig 0.15.2 —
same version as upstream. Two jobs cover the Ghostty build:

- **`zig-checked`** on `macos-latest`: runs
  `zig build -Dapp-runtime=none -Demit-xcframework=false -Demit-macos-app=false`.
  Fast gate, catches rebrand breakage, API drift, and .zon
  fingerprint mismatches. Uses the default Xcode 16 SDK on the
  runner; does not require macOS 26.

- **`zig-full`** on `macos-26-xlarge`: pins Xcode 26 via
  `xcode-select`, then runs `zig build -Dapp-runtime=none test`.
  Mirrors upstream's `nix develop -c zig build -Dapp-runtime=none test`
  job minus the nix wrapper (we have no CACHIX account and beta
  runners don't have flakes). Full unit-test suite.

---

## What NOT to do

- **Do not** add a second `origin` remote pointing to a personal fork.
  We do not publish a fork.
- **Do not** run `git push` from inside `sylvander-ghostty/` — there is
  nowhere to push to.
- **Do not** `git subtree split` to extract a separate history.
  We are integrated on purpose.