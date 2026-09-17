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
    echo "NOTE: this is a shallow clone, so bug-lint rule 6 runs only over the rows"
    echo "this tree changes and its full-tree audit is skipped — git cannot resolve"
    echo "historical SHAs. Rule 6b runs at any depth and is the check that catches the"
    echo "case this hook exists for: a fix SHA a rebase orphaned, which still RESOLVES"
    echo "in the clone that made it. Run 'git fetch --unshallow' for rule 6 in full."
fi
