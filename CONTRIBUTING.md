# Contributing to af (agentflow)

Thanks for contributing! `af` is a Rust orchestrator that supervises worker
agents. This guide covers building, testing, coverage, and the spec-driven
workflow that every change goes through.

## Toolchain

The Rust toolchain is pinned in `rust-toolchain.toml` (stable release plus
`rustfmt` and `clippy`). Any rustup-backed install picks it up automatically —
no manual version juggling. `cargo llvm-cov` is needed only for the coverage
gate (see below).

## Build

```sh
cargo build --release
```

This produces the `af` binary at `target/release/af`. The repo also builds an
`example_agent` binary (a stub worker used by tests and local experiments):

```sh
cargo build --release --bin example_agent
```

Convenience targets: `make build-release`, `make install` (copies `af` to
`$(PREFIX)/bin`).

## Tests

```sh
cargo test
```

runs the full suite — unit tests in `src/` plus the integration and E2E tests
in `tests/`. The E2E tests exercise the real `af` binary against a stub agent
(the bundled `example_agent` / an in-process stub server) and throwaway
scratch git repositories created in temp dirs. **No network access and no LLM
API keys are required**; everything runs offline.

Before pushing, also run the lint gate that CI enforces:

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
```

(`make check` runs fmt + clippy + tests together.)

## Coverage

CI enforces a **line-coverage floor** via the `coverage` job:

```sh
./scripts/coverage-gate.sh
```

The script reads the floor from `.coverage-min` (a single number — the single
source of truth) and fails if total workspace line coverage drops below it
(`cargo llvm-cov --workspace --fail-under-lines "$MIN"`).

The floor is a **ratchet**: it may only be raised over time, never lowered
silently to make a build pass. If your change adds uncovered lines, add tests
rather than touching `.coverage-min`; relaxing the floor is a deliberate,
reviewed trade-off only. Details: `docs/coverage.md`.

Run it locally the same way — it works from any working directory.

## Spec-driven development (openspec)

Changes are specified **before** they are implemented. `openspec/` is the
single source of truth for behavior:

- `openspec/specs/` — current capability specs (per area: `cli`, `config`,
  `lifecycle`, `sandbox`, `scheduling`, `state`, `worktree`).
- `openspec/changes/<date>-<slug>/` — one directory per in-flight change,
  containing:
  - `proposal.md` — why, what changes, and which capabilities are affected;
  - `specs/` — spec deltas for the affected capabilities;
  - `tasks.md` — the implementation checklist (ticked as work lands);
  - `design.md` — optional, for non-obvious approaches.

Workflow: open the change directory with a proposal and spec deltas first,
get agreement, then implement, ticking tasks and adding `// spec:` markers in
tests that cover each requirement (see `docs/spec-traceability.md`). Merged
change directories are moved to `openspec/changes/archive/`.

## Architecture documentation

Architecture documentation lives in `docs/arc42/`, following the arc42
template (chapters 01–12: goals, constraints, context, solution strategy,
building-block and runtime views, deployment, concepts, decisions, quality
requirements, risks, glossary). If your change alters the building-block or
runtime view, update the relevant chapter in the same PR. Other docs:
`docs/coverage.md` (coverage ratchet), `docs/spec-traceability.md`
(spec-to-test traceability).

## Pull requests

1. Fork/branch, keep the change focused, and follow the openspec flow above.
2. `make check` green, and `./scripts/coverage-gate.sh` at or above the floor.
3. Describe what changed and link the openspec change directory.
