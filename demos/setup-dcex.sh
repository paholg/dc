#!/usr/bin/env bash
set -euo pipefail

# Prepare fresh CLI + Proxy
just proxy-up
cargo build -q --bin devconcurrent

source "$(dirname "${BASH_SOURCE[0]}")/env.sh"

# Fetch and reset `dcex` to main, and clean-up anything left behind.

[ -d "$EXAMPLE_DIR/.git" ] || git clone -q "$EXAMPLE_REPO" "$EXAMPLE_DIR"
git -C "$EXAMPLE_DIR" fetch -q origin main

# Destroy leftover workspaces before resetting the checkout; `destroy` takes the
# worktree with it, and git can't remove a worktree it no longer knows about.
# The first entry `worktree list` prints is the checkout itself.
while read -r worktree; do
    devconcurrent destroy --force "$(basename "$worktree")"
done < <(git -C "$EXAMPLE_DIR" worktree list --porcelain |
    awk '/^worktree /{print $2}' | tail -n +2)

git -C "$EXAMPLE_DIR" worktree prune
git -C "$EXAMPLE_DIR" switch -qC main origin/main
git -C "$EXAMPLE_DIR" clean -qfd

# Drop leftover branches; `main` is checked out, so it can't be deleted. The
# process substitution keeps `grep` finding nothing from tripping `pipefail`.
while read -r branch; do
    git -C "$EXAMPLE_DIR" branch -qD "$branch"
done < <(git -C "$EXAMPLE_DIR" branch --format='%(refname:short)' | grep -vx main)

devconcurrent up dcex
