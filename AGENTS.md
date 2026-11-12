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

# Testing policy

- Keep unit tests in a `#[cfg(test)] mod tests` at the end of the implementation
  file by default. Extract tests or shared helpers when readability warrants it;
  do not create a matching test directory for every module automatically.
- Put public-API integration tests in each crate's `tests/` directory. Do not
  make internal APIs public solely to accommodate tests.
- Test behavior, contracts, and bug regressions. Avoid mirroring implementation
  details or retesting dependencies without exercising this crate's behavior.
  Choose tests by their value, not fixed test-count, line-count, or coverage quotas.
- Limit fakes and mocks to the external boundaries needed by the test. Do not
  mechanically expand them into substitutes for the whole Vulkan API.
- Prioritize synchronization, lifetimes, destruction, and failure paths. Weigh
  unusual defensive cases by realistic failure paths, impact, and the setup and
  maintenance cost of the test.
- Keep CPU tests separate from real Vulkan tests. Mark real Vulkan tests
  `#[ignore]` and run them explicitly; missing runtime prerequisites must fail
  rather than silently skip. Require zero validation warnings and errors, with
  only individually documented, confirmed false positives eligible for exceptions.
  Keep excepted messages in logs; follow [the development guide](docs/guides/development.md).

# Documentation

- Keep current documentation under `docs/guides/` and `docs/design/`, with the
  latest verification results in `docs/verification/current.md`.
- Update existing topic documents rather than adding milestone baseline reports
  or chronological ADR files. Keep superseded content in Git history.
- Maintain [the documentation index](docs/README.md) and relative links when
  adding, consolidating, or moving documents. Track tasks in Beads.
