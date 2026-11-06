# Proposal: cascade-routing

## Why

The router is a single UCB1 scoring machine across every task. That is
right for a homogeneous queue, but a mixed workload wants cost-first
escalation: start the cheap model, escalate to the capable one only when
the acceptance gate says the work is wrong. The corpus synthesis grounded
this in three papers: APR-Router (route by complexity, escalate on
failure), LogRouter (two-level cost-aware routing that spends cheap first),
and "Knowing When a Cheap Model Is Enough" (a cheap model wins until the
gate disagrees). agentflow already has the two ingredients — per-attempt
receipts and a gate as sole arbiter — but retries go back through the
global UCB1 pick, so a task cannot say "this one escalates: flash → pro →
opus".

## What Changes

- **Per-task `cascade` field** on a Task: an ordered list of worker names,
  cheapest first. Attempt `N` dispatches on `cascade[N-1]`.
- **Fallback to UCB1** when the cascade worker is busy, disabled, or the
  attempt number is past the cascade — a cheap busy worker never stalls the
  pipeline.
- **Validation**: cascade entries must name real workers, be unique within
  the cascade, and the array must not be empty; a cascade starting on a
  disabled worker warns non-fatally.
- The gate stays the sole arbiter — cascade changes WHO is dispatched, never
  whether work is accepted.

## Capabilities

- **New Capabilities**:
  - `openspec/specs/scheduling/spec.md` — "Cascade (escalation) routing".
- **Modified Capabilities**:
  - `openspec/specs/config/spec.md` — task field schema documents `cascade`.

## Impact

- `src/config.rs` — `Task::cascade: Option<Vec<String>>` (serde default
  None) + validation.
- `src/run.rs` — `cascade_worker` helper; dispatch consults it before
  `pick_worker`.
- Tests: config validation (unknown worker / duplicate / empty / disabled
  warning), `cascade_worker` unit (attempt indexing, busy/disabled fallback,
  no-cascade passthrough).
- README: task schema + routing doc.
- No new dependencies.

## Design decisions

- **Attempt-indexed, not state-indexed.** The attempt number is the
  escalation level: attempt 1 = cascade[0], attempt N = cascade[N-1].
  Simple, deterministic, and replays from receipts if the orchestrator dies.
- **Busy falls back, does not wait.** Stalling an escalation chain behind a
  busy cheap worker would serialize the fleet on the cheapest rung. The
  fallback keeps parallel progress; the next attempt still escalates.
- **Gate = sole arbiter.** Cascade never skips or weakens the acceptance
  gate; it only chooses who gets the next shot. This is what keeps the
  ceiling honest — escalation is driven by the gate's verdict, not by a
  heuristic.
