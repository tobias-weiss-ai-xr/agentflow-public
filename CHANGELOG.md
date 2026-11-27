# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0] - 2026-11-27

### Added

- **Run module split** — `src/run.rs` factored into `src/run/` (`mod`, `cost`,
  `dry_run`, `loop`, `recover`, `results`, `reuse`, `status`); behavior unchanged.
- **Usable model list** — `docs/workers.md` (PROVEN / CONDITIONAL / EXCLUDED with
  one evidence line per model) plus `KNOWN_PROBLEM_MODELS` in `src/config.rs`:
  `af validate` now warns (never blocks) when an enabled worker uses an
  excluded model id; `max_in_flight` documented (mistral 2 lanes, glm-5.2-awq
  serialized).

### Fixed

- **Failure receipts carry the agent's own diagnosis.** A CLI worker that
  exits non-zero with no committed work now records `agent exited NonZero
  (code 1); last output: <cause line>` — the first output line that names an
  infra signal (CLIs print the cause first, then remediation hints and a
  docs path), else the last line — and a worker that wrote nothing records
  `agent produced no output (exit N) after Ts`. `classify_failure` learns the
  vocabulary agents actually emit (`aborted`, `api key`, `not found for
  provider`, `429`, `503`, `rate limit`, `overloaded`, `socket`, `eof`,
  `reset by peer`, `dns`, `empty response`, `no output`), so transport and
  configuration faults classify as `infra` and no longer lower worker trust.
  Re-running the classifier over the archived port-1000 receipts
  (240 receipts; 190 `agent` / 38 `infra` as recorded) moves **168 → infra**
  (22 `agent` / 206 `infra`). The 164 identical 5-second deaths behind it
  were a misconfigured provider (`No API key found for openai.`), invisible
  for a day behind `code 1`.
- **`af` never merges into a dirty checkout.** `worktree::merge` pre-flights
  the merge target and refuses with `merge target is dirty (<path>); commit
  or stash before af merges` — nothing runs, the operator's tree is untouched,
  the attempt branch is kept for `af recover`. The dispatch loop parks a
  task whose merge target is dirty (no receipt, no attempt spent; logged
  once; `af status` shows `waiting: merge target dirty (<path>)`) and
  resumes automatically once the checkout is clean.

## [0.1.1] - 2026-11-09

### Added

- Add `docs/recipes.md` — campaign recipes cookbook with ready-to-run orchestration patterns.
- Add `docs/troubleshooting.md` — guide to common failure modes and their remedies.
- Add `docs/quickstart.md` — expanded five-minute walkthrough from install to first merged campaign.
- Add `CONTRIBUTING.md` — contributor guide covering build, test, coverage, and openspec workflow.
- Add `docs/diagrams/campaign-flow.md` — mermaid diagram of the campaign lifecycle.
- Add README mermaid pipeline flowchart and a `## Campaign monitoring` cheat-sheet section.
- Add `examples/simple/` — a complete minimal example campaign (`tasks.json`, `workers.json`, README).
- Add ADR-16 (code-graph-assisted task scoping) to `docs/arc42/09-architecture-decisions.md`.
- Add module-map declaration plus an `af validate` call-connected scope warning.
- Add transcript edge-case test coverage.
- Add worker cost-basis documentation.
- Add tag-triggered release workflow: x64 native + arm64 cross builds, tarballs + `SHA256SUMS`, release notes from this changelog.

### Fixed

- Fix `Config` test initializers missing the new `module_map` field (merge gap between `mm-config` and pre-existing tests).
- Fix `scripts/dist/` shallow clones fetching no tags (version fell back to bare `0.1.0`); dist builds are now produced by the release workflow.

## [0.1.0] - 2026-11-07

### Added

- **Parallel orchestration** — `af run` dispatches declarative tasks
  (`tasks.json`) to OpenAI-compatible workers in isolated git worktrees,
  one branch per task.
- **Exact acceptance gates** — every task carries an `accept` command that
  must exit `0` inside its worktree before a branch is considered for merge.
- **Automatic merge and archival** — a passing gate merges the branch into
  the base automatically; every failure path (stall, timeout, non-zero agent
  exit, scope violation, merge conflict, interruption) archives the branch
  as `<branch>-rejected-<ts>` instead of destroying paid-for work.
- **UCB1 trust routing** — workers are selected by UCB1 trust scoring, with
  a per-worker receipt after every attempt and an `af cost` ledger tracking
  spend, waste, and per-worker trust across campaigns.
- **Dependency DAG with contention avoidance** — tasks declare dependencies
  and scopes; tasks whose scope globs overlap are never dispatched at the
  same time.
- **Retry with failure memory** — a failed attempt retries up to
  `max_attempts`, and the retry prompt lists the earlier failures so the
  agent does not repeat them.
- **Multi-repo campaigns** — `repos.json` maps repository names to paths, so
  one campaign can span several repos with per-repo worktree, branch, and
  merge targets.
- **Builtin in-process agent harness** — the `cli: builtin` worker runs the
  agent loop in-process, no external agent CLI required.
- **Campaign budgets** — spend ceilings via `AF_MAX_WALL_CLOCK_S` (seconds)
  and `AF_MAX_TOKENS` (tokens), `0` = unlimited.
- **Branch recovery** — `af recover` re-checks and lands archived branch
  work; the next `af run` can also reuse archived branches from the same
  run without paying an agent twice.
- **Sandbox seam** — `AF_SANDBOX_CMD` prepends a sandbox command (e.g.
  `firejail --net=none`) to the agent argv.

### Changed

- N/A — initial release.

### Fixed

- N/A — initial release.

[unreleased]: https://github.com/tobias-weiss-ai-xr/agentflow-public/compare/v0.1.1...HEAD
[0.2.0]: https://github.com/tobias-weiss-ai-xr/agentflow-public/releases/tag/v0.2.0
[0.1.1]: https://github.com/tobias-weiss-ai-xr/agentflow-public/releases/tag/v0.1.1
[0.1.0]: https://github.com/tobias-weiss-ai-xr/agentflow-public/releases/tag/v0.1.0
