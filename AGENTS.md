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

# Git workflow

- Work directly on `main` by default. Before editing, fetch remote changes and
  bring `main` up to date with a fast-forward; preserve existing local changes.
- A request to implement a change or edit repository files includes the in-scope
  validation, documentation and Beads updates, integration into `main`, commit,
  push to `origin/main`, and CI verification. No separate instruction to merge
  or push is needed. Explicit user limits or requests to stop earlier take
  precedence. Read-only questions and reviews do not authorize repository edits
  or publishing changes.
- Normal flow: update `main`, make the requested changes, run applicable checks,
  commit, push `main`, and confirm CI. Fix in-scope CI failures before reporting
  completion; report any unresolved external blocker explicitly.
- Only one agent may modify, merge into, or push `main` at a time.
- For parallel work that edits files, give each worker a dedicated worktree and
  a `codex/<task>` branch based on the intended `main` commit. Do not check out
  `main` in multiple worktrees. Read-only investigation and review may share a
  checkout.
- Each worker validates and commits its assigned changes. One integration agent
  merges completed branches into a clean `main`, resolves conflicts, runs the
  applicable checks on the combined result, and pushes `main`. Workers do not
  independently merge into or push `main`; task branches normally stay local.
- Keep Beads issues and the database connection consistent across worktrees.
  The integration agent coordinates final issue closure and Dolt pull/push;
  do not assume Git worktrees copy or initialize the ignored Beads database.
- After integration and push are confirmed, remove task-owned worktrees and
  branches only when they contain no uncommitted or unmerged work to preserve.

# Vulkan implementation and review

- Before Vulkan-related implementation or review, including Graph lowering and
  runtime resource management, read the relevant sections of the
  [Vulkan best-practices guide](docs/guides/vulkan-best-practices.md) and the
  [current Vulkan design](docs/design/vulkan.md). Follow the applicable guidance.
- Satisfy applicable Vulkan and VMA API contracts and the current Renderer
  policies. Performance advice or a documented recommendation exception cannot
  waive synchronization, lifetime, safety, or validation requirements.
- Follow general recommendations by default. When choosing another approach,
  record the reason, alternatives, risks and supporting evidence in the relevant
  existing design document and Beads issue, proportionate to the change.
- Treat vendor-specific and conditional optimizations as measured choices.
  Confirm the required capabilities, verify correctness, and measure benefits
  and tradeoffs on the target workload and hardware before adopting them as an
  optimization. Keep experiments and unverified performance claims distinct.
- Apply guidance within the task's scope; future-feature sections do not require
  implementing those features. Preserve a correct baseline for comparisons.
- Check advice against the actual API version, enabled extensions/features and
  pinned bindings/VMA. Resolve conflicts using applicable primary-source
  contracts and update stale guidance, current design and the Beads issue.
  Changes to Renderer policy require an explicit documented rationale; existing
  code or old documentation does not override API requirements.
- At completion, record applicable guidance, justified departures from general
  recommendations, and executed/unexecuted verification in the existing issue
  and relevant documentation. Use the established quality and validation gates.

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
