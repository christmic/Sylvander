#!/usr/bin/env bash
# sync-ghostty-subtree.sh — pull upstream ghostty into sylvander-ghostty/
# subtree, then drop the files we explicitly don't want in the fork.
#
# Usage:
#   ./scripts/sync-ghostty-subtree.sh           # pull + clean
#   ./scripts/sync-ghostty-subtree.sh --dry-run  # show what would happen
#
# Exit codes:
#   0   success
#   1   upstream pull failed (unresolved conflicts or network)
#   2   cleanup step failed
#
# Why this script exists:
#   The Sylvander repo embeds ghostty as a git subtree. Upstream's
#   CI configuration, issue templates, and assorted metadata (anything
#   under `sylvander-ghostty/.github/`, `sylvander-ghostty/PACKAGING.md`,
#   etc.) is for the *upstream* project, not the fork. Every time we
#   pull upstream, that file tree reappears. We drop it here.
#
# Strategy:
#   1. Run `git subtree pull` (squashed) — this brings in upstream.
#   2. If `sylvander-ghostty/.github/` reappears (it always will),
#      `git rm -r` it, then `git commit --amend` to fold the cleanup
#      into the squash commit. Result: a single merge commit.
#
# Manual review:
#   After running, check the diff. The squash commit is the only one
#   in the merge; you should see one diff with both the upstream
#   changes AND the missing `.github/` removal. Inspect the removal:
#   any file ghostty added that we genuinely want should be brought
#   back manually (e.g., a bug fix to Zig code) — but `.github/`-ish
#   artifacts are always wrong.

set -euo pipefail

DRY_RUN=false
if [ "${1:-}" = "--dry-run" ]; then
  DRY_RUN=true
fi

PREFIX="sylvander-ghostty"
UPSTREAM_REMOTE="ghostty-org/ghostty"
# Upstream default branch is `main` (as of ghostty 1.2+; was `master`
# earlier — override via `SYNC_UPSTREAM_BRANCH=master` if you need to
# pin to the old branch). The merge-commit subject the post-pull
# amend step looks for is `Merge branch '<remote>/<branch>'` so this
# string is the single source of truth for the branch name.
UPSTREAM_BRANCH="${SYNC_UPSTREAM_BRANCH:-main}"

# Files / directories we explicitly drop from the subtree. The rule
# is: anything that's about how ghostty-the-upstream-project is run
# (CI, issue triage, community governance, release plumbing, AI
# agent tooling) is not applicable to Sylvander-the-fork.
DROP_PATHS=(
  "$PREFIX/.github"
  "$PREFIX/.agents"             # ghostty-org's Claude Code skills (empty placeholders today; not ours)
  "$PREFIX/PACKAGING.md"
  "$PREFIX/HACKING.md"
  "$PREFIX/CONTRIBUTING.md"
  "$PREFIX/AI_POLICY.md"
  "$PREFIX/issue-unvouched-message"
  "$PREFIX/VOUCHED.td"
  "$PREFIX/dist/cmake"
)

# Also: standalone scripts that don't apply to a non-CI consumer.
DROP_SCRIPTS=(
  "$PREFIX/.github/scripts/check-translations.sh"
  "$PREFIX/.github/scripts/ghostty-tip"
)

# Sanity: refuse to run outside a Sylvander checkout.
if [ ! -d "$PREFIX" ]; then
  echo "ERROR: '$PREFIX' not found. Run from the Sylvander repo root." >&2
  exit 1
fi

if [ "$(git rev-parse --show-toplevel)" != "$(pwd)" ]; then
  echo "ERROR: must run from the Sylvander repo root, not a subdir." >&2
  exit 1
fi

# Sanity: working tree must be clean — refuse to mix an in-progress
# subtree pull with unrelated changes.
if ! git diff --quiet HEAD; then
  echo "ERROR: working tree is dirty. Commit or stash before syncing." >&2
  exit 1
fi

# Fetch upstream refs.
echo "==> Fetching $UPSTREAM_REMOTE"
if [ "$DRY_RUN" = true ]; then
  echo "    (dry-run: skipping actual fetch)"
else
  git fetch "$UPSTREAM_REMOTE" "$UPSTREAM_BRANCH" \
    || { echo "ERROR: fetch failed" >&2; exit 1; }
fi

# Pull the subtree (squashed).
echo "==> Pulling subtree (squashed) into $PREFIX/"
if [ "$DRY_RUN" = true ]; then
  echo "    (dry-run: would run: git subtree pull --prefix=$PREFIX $UPSTREAM_REMOTE $UPSTREAM_BRANCH --squash)"
  exit 0
fi

# `git subtree pull` may open an editor for the merge commit message.
# Tell git to use a no-op editor so the script is non-interactive.
GIT_EDITOR=":" git subtree pull --prefix="$PREFIX" \
  "$UPSTREAM_REMOTE" "$UPSTREAM_BRANCH" --squash \
  || { echo "ERROR: subtree pull failed (likely a conflict in $PREFIX/; resolve manually, then re-run)" >&2; exit 1; }

# Now drop the unwanted paths. If they don't exist (first run after
# the script was introduced), `git rm` errors out — ignore those.
echo "==> Dropping upstream-only files"
for path in "${DROP_PATHS[@]}" "${DROP_SCRIPTS[@]}"; do
  if git ls-files --error-unmatch "$path" >/dev/null 2>&1; then
    echo "    rm -r $path"
    git rm -r "$path" || { echo "ERROR: failed to git rm $path" >&2; exit 2; }
  else
    echo "    (skip $path — not tracked)"
  fi
done

# `git status` to confirm the only diff is the removal we want.
echo
echo "==> Pending changes:"
git status --short

# If there's anything to commit, decide whether to amend the
# most-recent commit (the subtree squash) or to make a new commit.
#
# Cases:
#   1. HEAD's most-recent commit is a `Merge branch 'ghostty-org/...'
#      into …` (the subtree squash just produced it): amend it so
#      the PR shows one commit instead of two.
#   2. HEAD's most-recent commit is anything else (e.g., a regular
#      feature commit made since the last sync): make a new commit.
#      A `--fixup` style would also work, but explicit is simpler
#      and the diff is small.
if git diff --cached --quiet; then
  echo "    (no cached changes — nothing to commit)"
else
  if git log -1 --format=%s HEAD | grep -q "Merge branch 'ghostty-org"; then
    echo "==> Amending the subtree squash commit to include removals"
    git commit --amend --no-edit
  else
    echo "==> Creating a new commit for the upstream-only removals"
    git commit -m "chore: drop upstream-only files after sync

Removed by scripts/sync-ghostty-subtree.sh. See sylvander-ghostty/SYNCUP.md
§7.1 for the policy. Run again next time you sync."
  fi
  echo
  echo "==> Final commit:"
  git log -1 --stat
fi

echo
echo "Done. Review the diff, then push:"
echo "  git push origin master"
