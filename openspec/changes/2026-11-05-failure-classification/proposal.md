# Proposal: failure-classification

## Why

agentflow records every failed attempt as a `failed` receipt, and every
such receipt is a VERDICT on the worker that ran it. But not all failures
are the worker's fault: a provider 429, a connect error, a timeout, a stall
watchdog kill, a missing binary, or a native-harness turn cap happen no
matter how good the model is. Today those infra failures silently lower the
worker's UCB1 trust — the router learns "this worker is unreliable" from an
incident the infrastructure caused. The feedback doc (§3) asks for exactly
this classification, and it is still unimplemented. The "From Failed
Trajectories" paper (arXiv 2606.06324) frames the same gap: harness flaws
and agent failures must be diagnosed separately or the harness mis-attributes
blame and tunes the wrong thing.

## What Changes

- **Receipts carry a failure class.** A `failed` receipt records
  `failure_class`: `"infra"` (harness, transport, or environment — provider
  error, timeout, stall, missing binary, kill, git/io error, native-harness
  turn cap) or `"agent"` (the work itself was judged wrong — non-zero exit,
  gate failure, scope violation, zero commits, merge-verification failure).
  `None` for merged/verified, interrupted/recovered bookkeeping receipts,
  and legacy receipts (serde default — backward compatible).
- **Trust stops eating infra blame.** `Receipt::counts_as_verdict` is false
  for an `infra`-classified failed receipt, so the UCB1 router, the live
  `reap` update, and `af cost`'s `WINS/TOTAL` all exclude it from both the
  numerator and the denominator. An infra failure still counts against the
  task's `attempts`/`max_attempts` retry budget (the attempt genuinely
  happened) and toward cost totals (the tokens were spent) — only the trust
  statistic ignores it.
- **Visibility.** `af status --json` surfaces each task's most recent
  `failure_class`. `af cost`'s `WASTED BY REASON` already shows infra causes
  via the error text; the class adds a trustworthy one-line signal.

## Capabilities

- **New Capabilities**:
  - `openspec/specs/state/spec.md` — "Failure class on receipts".
- **Modified Capabilities**:
  - none.

## Impact

- `src/state.rs`: `Receipt.failure_class` field (serde default), and
  `counts_as_verdict` excludes `"infra"`.
- `src/execute.rs`: `classify_failure(&str) -> &'static str` — the single
  source of truth, matching agentflow's controlled failure-reason prefixes;
  set on the receipt from the full reason before truncation.
- `src/run.rs`: `reap` skips the live router update for infra failures;
  `status_json` surfaces `failure_class`.
- Tests: classifier unit tests (infra vs agent signals), counts_as_verdict
  infra/agent/legacy, router trust not lowered by infra, status_json surface.
- docs: README (§3 answer — infra failures no longer poison trust).
- No new dependencies.

## Design decisions

- **One classifier, two call sites.** `classify_failure` runs on the SAME
  reason string in `execute_task` (persisted receipt) and `reap` (live
  router), so replay and runtime agree by construction. It matches the
  controlled prefixes agentflow already produces (`agent exited Timeout`,
  `agent stalled:`, `harness:`, `cannot …`, `gate failed`, `scope
  violation`, `zero commits`, …).
- **Turn cap is `infra`.** The turn-cap escalation feature already treats a
  turn-cap death as a budget wall to escalate from, not a capability
  verdict — classifying it `infra` is consistent with that operational
  stance and with the "don't blame the worker for the harness budget"
  principle.
- **A CLI agent's plain non-zero exit is `agent`.** For a CLI worker we
  cannot see why it exited; the conservative, measurable choice is to treat
  it as a verdict (the fallback), so trust data stays honest — only clearly
  environmental signals get the `infra` exemption.
- **Backward compatible.** Legacy failed receipts deserialize with
  `failure_class: None` and keep today's verdict semantics exactly; merged
  receipts are untouched.
