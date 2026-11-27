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

# CI execution time

- Aim for full CI on `main` to finish within **5 minutes with warm caches**.
  Measure wall time from workflow start until the last required check finishes,
  including time spent waiting for runners.
- Before changing a workflow or a script it runs, use a recent comparable run
  to identify which jobs determine completion time. Preserve required platform,
  feature and test coverage. Reuse existing builds and pinned tools, and account
  for runner availability when splitting jobs or adding matrix entries.
- After the change, compare actual GitHub Actions runs with equivalent coverage.
  If cache keys or setup changed, report the first run and a warm run separately.
  Record run links, elapsed times and the limiting job in the Beads issue.
  Investigate regressions even within the five-minute target. If a slowdown is
  necessary for coverage, document its cost and the reason for keeping it.

# Vulkan implementation and review

- Before Vulkan-related implementation or review, including Graph lowering and
  runtime resource management, read the relevant sections of the
  [Vulkan best-practices guide](docs/guides/vulkan-best-practices.md) and the
  [current Vulkan design](docs/design/vulkan.md). Follow the applicable guidance.
- Satisfy applicable Vulkan and VMA API contracts and the current Renderer
  policies. Performance advice or a documented recommendation exception cannot
  waive API, synchronization, lifetime, or safety contracts. The placement of
  checks follows the production and development validation policy below.
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

# Production and development validation

- Before adding, moving, disabling, or reviewing validation, read the
  [validation policy](docs/guides/validation-policy.md). It governs asset input,
  Rust API contracts, RenderGraph, and all backends.
- Shipping draw/submit paths must not repeatedly validate already guaranteed
  contents. Establish guarantees at cook/import, creation/update, or Graph
  construction boundaries; preserve them through private immutable data,
  ownership, and explicit invalidation where applicable.
- Compile development-only diagnostics out of shipping configurations, including
  their dedicated scans, metadata, retained payloads, strings, and dependencies.
  Keep optimization and validation independently selectable. Do not assume that
  a release profile, hidden logs, or disabled native layers removes these costs.
- Preserve safe Rust and native API contracts in every configuration. Do not
  replace required checks with debug assertions without another guarantee, and
  never put required side effects inside validation-only code. Use a documented
  unsafe boundary only when the caller must assume safety obligations.
- Keep execution planning, synchronization, GPU lifetime/completion, capacity
  management, and native failure handling in shipping. Eliminate redundant state
  checks only when ownership, immutability, or another explicit invariant makes
  them unnecessary. Keep optional profiling and diagnostics independent.
- For changes to validation paths, verify applicable enabled and disabled
  configurations, including optimized execution; preserve existing real-GPU
  acceptance gates. Record the guarantee, invalidation, removed work, evidence,
  and unexecuted checks in the relevant design document and Beads issue.
- Distinguish this policy from implementation status. Current native-layer
  controls do not remove Renderer-side checks; document gaps rather than
  claiming that a shipping configuration already implements the policy.

# Steady-state frame memory

- Design normal frame processing for zero general-purpose CPU heap allocations,
  reallocations, and deallocations, and zero Renderer-issued native resource or
  memory creation/destruction. Follow the scope and evidence requirements in
  [the frame memory policy](docs/design/implementation-policy.md#steady-state-memory).
- Cover Graph build/compile, prepare, record, submit, poll/retire, present, and
  result consumption/return within prepared resource and capacity limits, even
  when per-frame inputs change. Moving the same work to another thread or API
  does not remove it from this scope.
- Reserve capacity at explicit preparation/change boundaries and reuse arrays,
  workspaces, and pools. Reserving/returning slots or suballocating within that
  capacity is allowed; its bookkeeping must also avoid heap growth and release.
  Do not silently grow pools or add implicit GPU waits on ordinary exhaustion.
- Reuse only after all recording, GPU, presentation, and CPU consumers relevant
  to that resource have finished. The zero-allocation goal cannot waive safe
  Rust/native contracts, synchronization, capacity checks, or failure handling.
- Measure allocation, reallocation, and deallocation separately from native
  create/destroy calls. Keep optional diagnostics and unobservable driver-internal
  allocations distinct from the Renderer guarantee. Record remaining gaps in
  Beads and current verification; this design goal is not a claim of completion.

# API compatibility

- Backward compatibility is not a default constraint. Prefer the appropriate API
  and update in-repository consumers and documentation together; do not add
  compatibility wrappers or parallel APIs solely to preserve existing callers.
- If compatibility becomes a requirement, revise this policy explicitly before
  treating it as a constraint.

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

- Put new task-specific generated files under `build/tasks/<beads-id>/` in
  the working checkout. Reuse `target/` for Cargo builds. Keep outputs for
  open or paused work, including background jobs that outlive a session.
- Before closing an issue, preserve any evidence needed later in Git or a
  durable location linked from Beads and the relevant document. Once all
  consumers finish, the closed task's output directory is disposable.
  See the [retention workflow](docs/guides/development.md#課題別の生成物の保管と削除).

- Keep current documentation under `docs/guides/` and `docs/design/`, with the
  latest verification results in `docs/verification/current.md`.
- Update existing topic documents rather than adding milestone baseline reports
  or chronological ADR files. Keep superseded content in Git history.
- Maintain [the documentation index](docs/README.md) and relative links when
  adding, consolidating, or moving documents. Track tasks in Beads.
