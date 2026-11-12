# Issue tracking

This repository uses **Beads (`bd`)** for issue tracking. Read
[.beads/README.md](.beads/README.md) for installation and synchronization.

- Run `bd prime` at the start of a session for current workflow guidance.
- Run `bd ready` to find available work and `bd show <id>` to read its context.
- Create missing tasks with `bd create "Title" -t task -p 2 -d "Description"`.
- Claim work with `bd update <id> --claim` before making changes.
- Record dependencies with `bd dep add <blocked-id> <blocking-id>`.
- Record progress and discoveries in the issue; use `bd close <id>` when done.
- Use Beads rather than Markdown TODO lists for task tracking.
- The database uses embedded Dolt. Git tracks configuration and hooks; issue
  history is synchronized separately with `bd dolt pull` / `bd dolt push`.
- Commit and push only within the scope authorized by the user.

# Rust module layout

- Do not create or retain `mod.rs` files, including test modules.
- Use self-named module files: `foo.rs` defines `foo`, and its child modules
  belong under `foo/` (for example, `foo/bar.rs`).
- Keep `clippy::mod_module_files` denied through the workspace lint settings.
