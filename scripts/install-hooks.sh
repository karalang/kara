#!/usr/bin/env bash
# One-time per-clone setup: point git at the committed hooks/ directory so the
# pre-push bug-ledger lint runs before every push. `git clone` does not set
# core.hooksPath, so each clone opts in by running this once. Safe to re-run.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"
git config core.hooksPath hooks
echo "core.hooksPath set to 'hooks' — pre-push bug-ledger lint is now active."
echo "Disable with: git config --unset core.hooksPath"
if [ -f .git/shallow ]; then
    echo
    echo "NOTE: this is a shallow clone, so the fix-SHA resolvability check will be"
    echo "skipped (git cannot resolve historical SHAs). Run 'git fetch --unshallow'"
    echo "for the hook to catch dangling pre-rebase fix SHAs."
fi
