# Repository Delivery Rules

These rules define the default Git delivery workflow for this repository.

## Git workflow

- Work directly on the local `main` branch by default. Do not create another branch for routine implementation or review work.
- Commit completed, verified changes directly to `main` with focused commit messages.
- After verification, push the local `main` branch to `origin/main` using the repository owner's configured GitHub identity.
- Do not force-push, rewrite published history, or push to another remote unless the user explicitly requests it.
- Do not put GitHub tokens, private keys, signing keys, passwords, or other credentials in the repository. Use the local Git credential/SSH configuration for authentication.

## Before pushing

- Run the relevant Rust, Android, shell, release, and archive checks for the files changed.
- Confirm `git status` is clean except for changes intentionally included in the commit.
- Run `git diff --check` and inspect staged files before committing; stage files by purpose rather than using `git add .`.
- Confirm the target is `origin/main` and verify the remote commit after pushing.
- Preserve unmerged worktrees that contain changes. A clean worktree may only be treated as a cleanup candidate after confirming its branch is merged; never delete a worktree automatically.

## Repository hygiene

- Keep local build outputs, release staging directories, generated archives, signing material, and machine-specific files untracked.
- Do not use destructive cleanup commands such as `git reset --hard` or `git clean -fd` without explicit user approval.
