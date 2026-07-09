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

## What NOT to do

- **Do not** add a second `origin` remote pointing to a personal fork.
  We do not publish a fork.
- **Do not** run `git push` from inside `sylvander-ghostty/` — there is
  nowhere to push to.
- **Do not** `git subtree split` to extract a separate history.
  We are integrated on purpose.