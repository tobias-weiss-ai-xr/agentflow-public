# Tasks: failure-hardening-gates

TDD order: A (receipt tail) → B (defect detection) → C (adaptive backoff) →
D (cost column) → docs → gates. Every new requirement/scenario carries its
`// spec:` marker (see `tests/spec_traceability.rs`). Verify locally with
`cargo test` in the relevant module before the full gates in section 6.

## 1. Receipt tail for gate failures (A)

- [x] 1.1 `gate_error_tail(&str) -> String` helper in `src/execute.rs`:
      last up-to-200 chars on line boundaries; empty → `(gate produced no
      output; exit N)` marker. Unit tests (RED first), incl. banner-then-
      failure and silent-nonzero cases.
- [x] 1.2 wire the truncation site (`src/execute.rs:157`): when the reason
      starts `acceptance gate failed`, use the tail; otherwise keep the
      first-line rule. Unit tests for both branches. Markers:
      - `// spec: state/cost-receipts#gate-failure-receipts-carry-the-failing-tail`
      - `// spec: state/cost-receipts#silent-gate-failure-names-the-missing-output`
- [x] 1.3 `tests/contract_prompt.rs` (or `failure_context` unit test):
      a retry prompt quotes the gate tail; a first attempt / a non-gate
      failure render the block unchanged. Markers:
      - `// spec: lifecycle/retry-prompts-reuse-the-gate-failure-receipt#retry-prompt-shows-the-gates-failing-line`
      - `// spec: lifecycle/retry-prompts-reuse-the-gate-failure-receipt#first-attempt-and-non-gate-failures-are-unchanged`
  - `cargo test` for `execute.rs` + contract modules green.

## 2. Repeated identical gate failure stops early (B)

- [x] 2.1 `gate_fingerprint(&str) -> Option<(String, String)>` (exit code +
      whitespace-normalized tail); `None` for non-gate errors. Unit tests
      (RED first): identical inputs → equal fingerprint; banner whitespace
      differences → equal; different tails → different.
- [x] 2.2 `reap` early-stop: on a gate-failed outcome, walk the task's
      receipts back past gate-only reruns (wall_clock_s < 5 s with no
      turns) to the previous AGENT-driven gate failure; when the
      fingerprints match and `attempts ≥ 2`, set `TaskState::Failed` with
      reason `repeated identical gate failure (<tail>) — task design
      defect`, persist, print, and dispatch nothing further. Unit tests
      with synthetic receipts, incl. resume-survival (receipts persisted
      across two `run_loop` calls). Markers:
      - `// spec: lifecycle/gate-failure-defect-detection`
      - `// spec: lifecycle/gate-failure-defect-detection#identical-gate-failure-stops-before-the-retry-budget`
      - `// spec: lifecycle/gate-failure-defect-detection#a-different-fingerprint-retries-normally`
      - `// spec: lifecycle/gate-failure-defect-detection#a-gate-only-rerun-never-triggers-alone`
      - `// spec: lifecycle/gate-failure-defect-detection#detection-survives-resume`
- [x] 2.3 `cargo test run::tests` green (reap + defect suite).

## 3. Adaptive per-worker backoff (C)

- [x] 3.1 `worker_backoff` becomes `HashMap<String, BackoffState { until,
      level }>`; on infra failure escalate `until = now + min(30 <<
      (level-1), 300)`; on any verdict reset level 0; single-enabled-worker
      guard caps at 30 s. Unit tests (RED first): escalation 30→60→120→240→
      300; reset on success; solo-worker cap; non-verdict math unchanged.
      Markers:
      - `// spec: scheduling/adaptive-worker-backoff`
      - `// spec: scheduling/adaptive-worker-backoff#cooldown-escalates-on-consecutive-infra-failures`
      - `// spec: scheduling/adaptive-worker-backoff#one-success-resets-the-escalation`
      - `// spec: scheduling/adaptive-worker-backoff#a-solo-worker-is-never-locked-out-for-minutes`
      - `// spec: scheduling/adaptive-worker-backoff#infra-failures-stay-non-verdicts`
- [x] 3.2 `cargo test` (router/backoff + reap suite) green.

## 4. Infra-drift column in `af cost` (D)

- [x] 4.1 per-worker `INFRA` count (selected failed receipts with
      `failure_class == "infra"`) + `last_infra` error; render column after
      `MEAN_S` (`-` when 0) and footnote when any worker has infra > 0;
      all-healthy output byte-identical. Unit tests on the report builder
      (RED first). Markers:
      - `// spec: cli/worker-infra-drift-breakdown-in-the-cost-report`
      - `// spec: cli/worker-infra-drift-breakdown-in-the-cost-report#infra-failures-surface-per-worker`
      - `// spec: cli/worker-infra-drift-breakdown-in-the-cost-report#an-all-healthy-report-is-unchanged`
      - `// spec: cli/worker-infra-drift-breakdown-in-the-cost-report#worker-absence-from-the-config-stays-a-note-not-a-column`
- [x] 4.2 narrows with `--last`/`--since`/`--task` like the rest of the
      report; cost-module tests re-run green.

## 5. Docs

- [x] 5.1 README: receipt/status note that gate failures surface the gate
      tail; `af cost` INFRA column, per-worker footnote.
- [x] 5.2 `docs/arc42/` runtime view: gate-failure diagnosis (tail receipts,
      early stop, adaptive backoff) where the failure handling is described.
- [x] 5.3 `docs/troubleshooting.md`: "task marked task design defect" entry
      (what to do: fix the gate/config, don't just re-dispatch).

## 6. Gates + archive

- [x] 6.1 `cargo fmt --check`
- [x] 6.2 `cargo clippy --all-targets -- -D warnings`
- [x] 6.3 full `cargo test` green (incl. `tests/spec_traceability.rs` —
      markers resolve once the change is merged and specs archived)
- [x] 6.4 `scripts/coverage-gate.sh` green (add tests for new uncovered
      lines — never lower the floor)
- [x] 6.5 `openspec validate --all` green
- [ ] 6.6 archive the change (`openspec archive`, specs merged); `cargo test`
      still green after merge
