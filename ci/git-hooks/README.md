# Local git hooks

Tracked git hooks for fbuild. Opt in **once per clone**:

```bash
git config core.hooksPath ci/git-hooks
```

After that, every hook in this directory is wired up automatically. The
directory is on the repo's normal source path so updates flow with
`git pull`.

## Hooks

- **`pre-push`** — workspace-wide safety net. Runs `cargo fmt --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`, and
  `cargo test --workspace` before any branch is pushed. Skipped when
  the push touches no Rust files. Bypass with `git push --no-verify`
  or `FBUILD_SKIP_PRE_PUSH=1 git push`.

  Why here and not in the Stop hook: the Claude Code Stop hook
  (`ci/hooks/check-on-stop.py`) is scoped to changed crates for fast
  iteration (#465). The workspace-wide gate belongs at the
  *push* boundary, where it actually prevents an escape, not at the
  conversation-stop boundary, where it just adds latency. See #462.

## Opting out per-push

```bash
git push --no-verify        # bypass all hooks
FBUILD_SKIP_PRE_PUSH=1 git push   # bypass only pre-push, keep others
```

## Adding a new hook

1. Drop an executable script named exactly the git event
   (`pre-commit`, `commit-msg`, `pre-push`, `post-merge`, ...).
2. Make it skip cleanly when there's no work to do — `pre-push` here
   filters by changed-file extensions so non-Rust pushes are free.
3. Document the bypass mechanism in the script header.
