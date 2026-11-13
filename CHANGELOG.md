# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.1] - 2026-11-12

### Added

- **Dynamic worker pool** — `AF_MAX_PARALLEL=0` (default) sets the pool to the current enabled-worker count, recomputed on every hot reload; no restart needed to grow or shrink the pool. An explicit `AF_MAX_PARALLEL=N` stays a fixed ceiling.
- **Per-task context** — `Task.context` (string or string array) renders into the agent prompt via `{{CONTEXT}}`; `{{#CONTEXT}}…{{/CONTEXT}}` conditional sections drop entirely when unset; templates omitting `{{CONTEXT}}` get an appended `## Task context` section + warning.
- **`AF_PROMPT_FILE` env var** — override the prompt-template path per campaign (legacy `TF_PROMPT_FILE` fallback with deprecation notice); template is re-read for every rendered prompt.
- **`{{PREVIOUS_ERROR}}`** — retry-failure block substitutes the prior attempt's error into the token; attempt 1 renders it empty.
- **Gate-failure tail on receipts** — gate errors now show the last ≤200 chars of gate output (boundary-aware) instead of the first line; silent gates get `(gate produced no output; exit N)`.
- **Repeated identical gate failure stops early** — two matching gate fingerprints (exit code + normalized tail) with `attempts ≥ 2` → `TaskState::Failed` with `task design defect` reason; detection uses persisted receipts and survives resume.
- **Adaptive per-worker backoff** — infra failures escalate 30→60→120→240→300 s; any verdict resets; solo worker capped at 30 s.
- **INFRA column in `af cost`** — per-worker infra-failure count + footnote; all-healthy report byte-identical.
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
- Add `docs/module-map.md` — module-map schema and `af validate` scope-contention usage.
- Add `examples/module-map.example.json` — example module map for ADR-16.
- Add `docs/postmortem-template.md` — template for campaign postmortems.

### Fixed

- Fix `Config` test initializers missing the new `module_map` field (merge gap between `mm-config` and pre-existing tests).
- Fix `scripts/dist/` shallow clones fetching no tags (version fell back to bare `0.1.0`); dist builds are now produced by the release workflow.
- Remove dead `{{EPISODES}}`/`{{MISSION}}` placeholders from shipped `prompts/worker.md`.

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

[unreleased]: https://github.com/tobias-weiss-ai-xr/agentflow/compare/v0.1.1...HEAD
[0.1.1]: https://github.com/tobias-weiss-ai-xr/agentflow/releases/tag/v0.1.1
[0.1.0]: https://github.com/tobias-weiss-ai-xr/agentflow/releases/tag/v0.1.0
