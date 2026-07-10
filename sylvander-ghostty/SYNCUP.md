# sylvander-ghostty — upstream sync & deep-fork workflow

This directory is a **git subtree mirror** of
[`ghostty-org/ghostty`](https://github.com/ghostty-org/ghostty),
embedded inside the `Sylvander` repository. We **deeply modify** it
so that Ghostty becomes Sylvander's native terminal frontend
(Sylvander tab → workbench UI → multi-session → native
notifications). We also keep in lock-step with upstream Ghostty
so we don't miss security fixes or features.

This document is the **only** guide for working in this subtree.
Read it before doing anything that touches files under
`sylvander-ghostty/`.

## 1. mental model — what a subtree is, what it isn't

| Question | Answer |
|---|---|
| Where is "our" copy of Ghostty hosted? | Local path `sylvander-ghostty/`. No GitHub fork. |
| How do we publish upstream? | **We don't.** No `git push` lands on `ghostty-org/ghostty`. |
| How do we receive upstream changes? | `git subtree pull` (this repo pulls from `https://github.com/ghostty-org/ghostty.git main`). |
| Where do OUR modifications live? | `src/sylvander/` (F2-F6 work), plus tiny patches to upstream files gated on `config.sylvander.enabled`. |
| Who owns the merge history? | The Sylvander repo (linear history; ghostty's 16 k+ commits are squashed into the `Add 'sylvander-ghostty/' from commit '…'` merge). |

**Implications**:
- You cannot "fork" Ghostty on GitHub without breaking the
  invariant. There is no separate history to push to.
- Cherry-picking fixes *back* upstream requires
  `git subtree split --prefix=sylvander-ghostty --branch ...`
  first, then exporting commits. We **do not** do this today;
  all patches stay local.
- Conflict resolution happens **inside this repo**, not on
  GitHub.

## 2. file map — read this before touching anything

```
sylvander-ghostty/
├── SYNCUP.md              ← you are here
├── build.zig             ← upstream; rarely edited (uses upstream step API)
├── build.zig.zon         ← deps + version; always upstream
├── src/
│   ├── sylvander/        ← OUR module (F2-F6 work goes here)
│   ├── build_config.zig  ← bundle_id + comptime config; PATCHED (Sylvander brand)
│   ├── build/            ← upstream build pipeline
│   ├── apprt/gtk/build/info.zig ← GTK app id; PATCHED
│   ├── main_c.zig …      ← upstream
├── macos/
│   ├── Sylvander-Info.plist   ← PATCHED file (renamed)
│   ├── Sylvander.sdef         ← PATCHED file (renamed)
│   ├── Ghostty.xcodeproj/    ← PATCHED refs (`INFOPLIST_KEY_CFBundleDisplayName`,
│   │                            `PRODUCT_BUNDLE_IDENTIFIER`)
│   ├── Sources/, Tests/     ← upstream Swift; keep imports of `Ghostty`
│   └── GhosttyKit.xcframework/  ← upstream name (Swift `import Ghostty`)
├── pkg/<*>/build.zig.zon  ← upstream; each is a wrapper around an upstream tarball
└── …                      ← upstream layout unchanged
```

**Files YOU may freely edit**:
- `src/sylvander/**` — Sylvander additions. Standard Zig module
  idioms; no upstream-style approval needed.
- `src/build_config.zig` — only `bundle_id` and possibly new
  comptime toggles.
- `src/apprt/gtk/build/info.zig` — only application id / resource
  path.

**Files that need an upstream-sync-aware diff**:
- Upstream Zig files. Any edit **must** be gated on
  `if (config.sylvander.enabled)` (or a similar flag) so a
  pristine upstream build still works for non-Sylvander users.
- Any CMake / Swift / Info.plist / sdef reference to `Ghostty` /
  `com.mitchellh.ghostty` that the upstream rename would clobber.

**Files you should NEVER edit directly**:
- `build.zig.zon` — the deps + `.name`/`.fingerprint` are
  upstream-anchored. Bumping these would invalidate our local
  Zig cache and run into `fingerprint mismatch` errors on the
  next sync. (If you genuinely need to, follow the "Adding a
  new dependency" section below.)
- `macos/Sources/**/*.swift` internal class names
  (`GhosttyScriptTab`, etc.) — these are referenced by `sdef`
  via `<cocoa class="GhosttyScriptTab"/>` and would require a
  coordinated Swift rename to change safely. Out of scope for
  F1.14 brand; revisit if a deep fork refactor requires it.
- `GhosttyKit.xcframework` — name needs to stay aligned with
  Swift `import Ghostty`.

## 3. routine sync — pull upstream changes

When ghostty-org publishes new commits, sync them in:

```sh
# from the Sylvander repo root
cd /Users/christmix/OraculoSpace/Sylvander

# First time (after F1.x):
git remote add subtree-remote https://github.com/ghostty-org/ghostty.git 2>/dev/null
git fetch subtree-remote main

# Then merge upstream into the subtree:
git subtree pull \
    --prefix=sylvander-ghostty \
    https://github.com/ghostty-org/ghostty.git main \
    --message="📦 sync: pull upstream ghostty main"
```

Conflict markers may appear inside any file we patched. The
typical cases are listed in §4.

After the sync:

```sh
# 1) Zig world: if upstream bumped any deps, .zon hash field
#    may now mismatch our cache. Easiest is to let `zig build`
#    re-fetch the few that changed (the rest stays in cache).
cd sylvander-ghostty
zig build -Doptimize=ReleaseFast --summary all
# 2) Manually inspect any new files inside src/build/ or macos/
#    that mention Ghostty branding.
# 3) If upstream renamed any reference paths (rare), re-run the
#    pbxproj syncup logic in §6.
```

### When to sync

- **On each F2+ feature branch start** — sync once to a clean
  upstream, then layer your feature on top.
- **Weekly** if you are doing bug-fix work that overlaps with
  upstream churn — `git log --oneline subtree-remote/main ^HEAD
  -- sylvander-ghostty/` gives you the queued commits.
- **Before any release build**. The release artifact must
  reflect the latest stable upstream line.

## 4. expected conflict scenarios and resolution

### 4.1 upstream edited `macos/*` Swift / Info.plist references to `Ghostty`

Upstream keeps the brand `Ghostty`, so any macOS-side edit
**only touches `Ghostty*` file refs**. Our PatchedFiles are
`Sylvander*` so the conflict is usually a textual conflict in
`project.pbxproj`.

**Resolution**:
- If upstream only added a new `INFOPLIST_KEY_CFBundleDisplayName`
  line, copy their one new line and change its RHS to
  `Sylvander` so we stay consistent.
- If upstream added a new PBXFileReference for a
  `Ghostty-Info.plist`, either (a) drop the upstream one and
  keep using ours, or (b) rename upstream's `Ghostty-Info.plist`
  → `Sylvander-Info.plist` and add a PBXFileReference for it.
  We currently use strategy (b) and rely on git rename
  detection.

### 4.2 upstream added / removed a `build.zig.zon` dep

`build.zig.zon` is **always upstream-merged verbatim**. The
Zig global cache stores packages by their `.hash` field, so
additions are no-ops in cache (zig fetches them on first
encounter) and removals just create dead cache dirs you can
`rm -rf ~/.cache/zig/p/<old-hash>` later.

### 4.3 upstream bumped `.name` / `.fingerprint` in `build.zig.zon`

Just before, our `.zon` had `.name = .ghostty, .fingerprint = 0x...`.
A new upstream release would have its own values. **Merge theirs
verbatim** — don't try to keep our `.name = .sylvander`. The
Sylvander brand lives in `src/build_config.zig`'s `bundle_id`
and in `pbxproj`'s `INFOPLIST_KEY_CFBundleDisplayName`/`PRODUCT_BUNDLE_IDENTIFIER`,
not in the package name (which only affects `~/.cache/zig/p/`
directory layout — an internal detail).

### 4.4 upstream renamed or moved a file inside `src/build/`

Examples: a `GhosttyLib.zig` split into multiple files, a file
moved out of `src/build/` into `src/build/<subdir>/`. We do
not currently have any *content* edits inside `src/build/`, so
this is mostly conflict-free. If upstream renames a file we
*do* edit (e.g. if F2 needs to change `src/build/Config.zig`),
use `git mv` to track the rename and re-apply our edit at the
new path. Re-run `zig build` to confirm.

### 4.5 upstream touched `src/build_config.zig`

We only edit the `bundle_id` line. If upstream rewrites the
file, keep our `bundle_id = "ai.oraculo.sylvander"` and let
the rest of their version in.

## 5. routine work — adding a Sylvander patch

### 5.1 inside `src/sylvander/` (preferred)

Drop new files there. The build system already `b.modules`
them. Update `src/sylvander/mod.zig` if you have a new public
type. Run:

```sh
cd sylvander-ghostty
zig build -Doptimize=ReleaseFast --summary all
./zig-out/Ghostty.app/Contents/MacOS/ghostty +version   # smoke test
```

Commit directly. No upstream coordination needed.

### 5.2 patching an upstream file (use sparingly)

If you need to patch something outside `src/sylvander/`:

```zig
// In the patched file:
const Config = @import("build_config.zig");

// In the function you are modifying, gate your addition:
if (Config.sylvander_enabled) {
    // Sylvander-specific behavior
} else {
    // Upstream fallback (always keep this branch working)
}
```

If adding the toggle is invasive, use the inverse gate:
`if (!Config.sylvander_enabled) return @call(.never_inline, oldFn(args));`

This keeps the patch surface minimal and makes upstream
re-sync a `git revert`-style affair.

### 5.3 adding a new build.zig.zon dep (rare)

If the new dep is `git+...`, `git+https://`, etc. — add to
`build.zig.zon` directly. **If** the new dep is a
`https://...tar.gz` and your network blocks Cloudflare CDN
(see §8), pre-fetch with `curl --proxy 127.0.0.1:1081` and
feed `zig fetch <local>` manually. Document the new URL in
§8 of this file.

## 6. file rename & pbxproj syncup recipe

We renamed `Ghostty-Info.plist → Sylvander-Info.plist` and
`Ghostty.sdef → Sylvander.sdef` (F1.14). If an upstream sync
adds a new file referenced from `pbxproj` that needs the
Silvander brand, repeat this ritual:

```sh
cd sylvander-ghostty/macos
# 1) rename the upstream-added file (preserve git history):
git mv Ghostty<New>.plist Sylvander<New>.plist
# 2) update pbxproj INFOPLIST_FILE / filename refs:
sed -i '' -e 's|Ghostty<New>\.plist|Sylvander<New>.plist|g' \
       Ghostty.xcodeproj/project.pbxproj
# 3) if it's an Info.plist, also patch CFBundleDisplayName:
sed -i '' -e 's|INFOPLIST_KEY_CFBundleDisplayName = Ghostty;|INFOPLIST_KEY_CFBundleDisplayName = Sylvander;|g' \
    -e 's|INFOPLIST_KEY_CFBundleDisplayName = "Ghostty<Variant>";|INFOPLIST_KEY_CFBundleDisplayName = "Sylvander<Variant>";|g' \
       Ghostty.xcodeproj/project.pbxproj
# 4) if it should set a Sylvander bundle id, also adjust
#    PRODUCT_BUNDLE_IDENTIFIER for the relevant target.
# 5) zig build -Doptimize=ReleaseFast --summary all
```

To verify the rename is recognized by git (so it shows as a
rename in `git log --follow`), let `git status` settle before
the commit: do the rename and the `sed` edit, then `git add -A`
and verify `git status` shows `R` (rename) instead of `D` + `?`.

## 7. CI

The CI lives at `../../../.github/workflows/ci.yml` (Sylvander
repo root), **not** inside this subtree. It contains four jobs:

- `zig-module` — `zig build test` on the Sylvander module
  subset. **Requires** `tests.linkLibC()` (we set this from
  F1.11; revert it and the run fails on macOS with
  `_realpath$DARWIN_EXTSN undefined`).
- `zig-checked` — `zig build -Dapp-runtime=none -Demit-xcframework=false -Demit-macos-app=false`. Fast gate on `macos-latest`. Validates our patches did not break the tree's compile path.
- `zig-full` — on `macos-26-xlarge`, pins Xcode 26 SDK then
  runs `zig build -Dapp-runtime=none test`. Mirrors upstream's
  `nix develop -c zig build …` test job, minus nix.
- `rust` — runs `cargo test --workspace` with five `--skip`
  patterns for streaming-event-contract tests that drift with
  mock responses.
- `macos-swift` — grep-checks that pbxproj doesn't reference
  the old `Ghostty-Info.plist` / `Ghostty.sdef` paths.

When we add or delete a file under `macos/`, **edit the
`macos-swift` job's grep list in lockstep**.

## 8. network — Cloudflare CDN, Sparkle mirror, prefetch

This subtree depends on downloading deps from
`deps.files.ghostty.org` (Cloudflare CDN). On the author's
network, **direct connections to Cloudflare are capped at
~5 KB/s**, but the local HTTP proxy `http://127.0.0.1:1081`
relays at ~5 MB/s. The Zig stdlib `Package.Fetch` does
**not** work correctly through that proxy (returns
`400 / 503`), and the upstream `nix develop` shell requires
Nix flakes we don't have.

### 8.1 first-time dep prefetch (or after wiping `~/.cache/zig/`)

```sh
# 1) pull a fresh list of .url= fields from build.zig.zon + pkg/*/build.zig.zon
URLS=$(find sylvander-ghostty/pkg sylvander-ghostty/build.zig.zon \
       -name 'build.zig.zon' -exec grep -hE '^\s*\.url\s*=' {} \; \
       | sed -E 's/.*"([^"]+)".*/\1/' | sort -u)

# 2) download all tarballs through the proxy
mkdir -p /tmp/sylvander-deps
while read -r url; do
  fname=$(basename "$url")
  curl -sL --proxy http://127.0.0.1:1081 --http1.1 --max-time 60 \
       -o "/tmp/sylvander-deps/$fname" "$url"
done <<< "$URLS"

# 3) feed each tarball through `zig fetch` to populate
#    ~/.cache/zig/p/<pkg>-<version>-<hash>/
for f in /tmp/sylvander-deps/*; do
  zig fetch "$f" || echo "FAIL: $f"
done
```

After this, `~/.cache/zig/` is fully populated and `zig build`
on any future re-run is fully cache-hit for the dep layer.

### 8.2 Sparkle workaround (macOS only)

`Ghostty.xcodeproj` references
`https://github.com/sparkle-project/Sparkle` as a SwiftPM
package. Xcode SPM internally uses git and does **not** honor
`~/.gitconfig` http.proxy. On this network, the git clone
times out.

We point Xcode at a local bare mirror:

```sh
# One-time setup (do this once per machine):
mkdir -p /tmp/sparkle-bare.git
git clone --bare \
    https://github.com/sparkle-project/Sparkle.git \
    /tmp/sparkle-bare.git
git config --global \
    url."file:///tmp/sparkle-bare.git".insteadOf \
    "https://github.com/sparkle-project/Sparkle"
```

The `insteadOf` rewrite is global and persistent. `git clone https://github.com/sparkle-project/Sparkle` will now fetch from
the local mirror. The clone target needs to be `https://...`,
which is what Xcode uses.

To refresh the mirror after upstream Sparkle releases:

```sh
cd /tmp/sparkle-bare.git
git fetch --force https://github.com/sparkle-project/Sparkle.git 'refs/heads/*:refs/heads/*' '+refs/tags/*:refs/tags/*'
```

### 8.3 Zig cache hygiene

The Zig global cache `~/.cache/zig/` is home to:
- `p/<pkg-hash>/` — extracted dep source trees. **Survives**
  between `zig build` invocations across the whole machine.
  Wipe only if upstream `.zon` hash conflict.
- `tmp/<randomhex>/` — partially-extracted tarballs from
  in-flight fetches. **Can be wiped at any time** during a
  stopped/canceled build without consequence.

Project-local cache `sylvander-ghostty/.zig-cache/` holds the
compiled `.o` and link artifacts. It's wiped by
`rm -rf .zig-cache` to force a fresh compile.

## 9. did upstream ship a change that's relevant to us?

When you scan upstream commits before a sync:

```sh
git log --oneline --stat subtree-remote/main ^HEAD -- sylvander-ghostty/
```

Particular files-of-interest for Sylvander:

| Path | Why we care |
|---|---|
| `macos/Ghostty*` / `src/build_config.zig` | rebrand collision — see §4.1, §6 |
| `pkg/<*>/build.zig.zon` URLs | network cache may go stale — see §8.1 |
| `src/build/Config.zig` | upstream may add new emit flags we'd want to mirror under our brand |
| `src/build/SharedDeps.zig` | new lazy deps (e.g. a new tool runtime). May impact our renderer integration |
| `src/terminal/`, `src/renderer/`, `src/font/` | our Sylvander tab will route into the renderer; review for ABI changes |

Upstream commits that have **no** changes inside `sylvander-ghostty/`
are not interesting for syncing this subtree (the Sylhand repo
might still pull them for the Rust crates that live at the top
of the monorepo, but those don't touch this subtree).

## 10. checklist before opening a PR against `christmic/Sylvander`

- [ ] `zig build -Doptimize=ReleaseFast` succeeds locally
      (`.app` appears in `zig-out/`).
- [ ] `./zig-out/Ghostty.app/Contents/MacOS/ghostty +version`
      returns a non-empty string.
- [ ] `plutil -p zig-out/Ghostty.app/Contents/Info.plist | grep
      CFBundleDisplayName` shows `Sylvander`.
- [ ] `git log --oneline origin/master..HEAD` lists exactly the
      commits you expect.
- [ ] No merge conflict markers (`<<<<<<<`, `=======`,
      `>>>>>>>`) anywhere under `sylvander-ghostty/`.
- [ ] `git subtree status --prefix=sylvander-ghostty` (or
      equivalent manual check) confirms we are not behind
      upstream by an uncomfortable gap.
- [ ] (If you edited macos/.pbxproj) `cd sylvander-ghostty/macos/Sylvander*.app/Contents/MacOS` shasum, compare with `xcodebuild -target Ghostty -configuration ReleaseLocal`, ensure LSP doesn't print errors.

## 11. we are not pushing back to ghostty-org

If, after working upstream-aware, you find a patch of yours is
genuinely a fix upstream would want (a real bug, not a brand
change or a Sylvander-specific binding), the procedure is:

```sh
git subtree split --prefix=sylvander-ghostty --branch to-upstream
# Then open a PR from `to-upstream` against ghostty-org/ghostty
# Once merged, `git branch -D to-upstream` and proceed with the
# next subtree pull as usual.
```

In practice, **none** of our patches so far are appropriate for
upstream (they're all about our branding and F2-F6 work). If
this changes, prefer the split-and-PR workflow over copying a
commit message by hand — it preserves attribution and the
upstream-merge commit gives us a clean handle to update.

## 12. failure recovery

If `zig build` starts emitting `undefined symbol _realpath$DARWIN_EXTSN`
or `fatal: unable to access 'https://github.com/...': Recv failure: Operation timed out`:

1. You are not on the documented Zig + Xcode toolchain
   (Zig 0.15.2 homebrew-patched + Xcode 26.x).
2. Otherwise, restart the build on Zig 0.15.2 with
   `brew link --overwrite zig@0.15` and re-pull deps via §8.1.

If `error: unable to unpack tarball to temporary directory: ReadFailed`:

- A background `curl` of the same tarball is racing with
  `zig fetch`. Kill the curl and let the next `zig build` retry.

If `error: the following build command failed with exit code 1: ... xcodebuild`:

- Run `tail -50` of the failure log. 80 % of the time it's
  a Swift import that broke because someone changed a
  GhosttyKit symbol without touching `GhosttyKit.xcframework`.
  Re-run `zig build --summary all` and look at the
  `+- xcodebuild failure` line for the specific step.

If `error: bad fingerprint`:

- Means our `.zon` and the on-disk cache disagree. Run
  `zig build` once more (it auto-recovers), or `rm -rf
  ~/.cache/zig/p && rm -rf sylvander-ghostty/.zig-cache` and
  re-prefetch per §8.1.

## 13. what is upstream-only-safe to edit on upstream files

For the very rare case where we want to keep an upstream file
edited but the upstream `git subtree pull` will try to clobber
us, prefer to put the edit under a feature flag. See §5.2.

If we cannot isolate behind a flag (e.g. a security-critical
patch that has to alter control flow directly), document the
patch in §9 so the next sync knows where the conflict is, and
adopt the conflict resolution in §4.
