# Proposal: failure-hardening-gates

## Why

A receipt audit across `state-*/` (151 receipts, 89 failed) shows exactly
where campaigns lose money and time, and the biggest lever is the
**acceptance gate**:

| class | receipts | what the data says |
|---|---|---|
| gate fail | 28 (31 %) | repeated *identically* across attempts on tasks with broken/unfixable gates |
| rate limit | 19 (21 %) | repeated 429s; the 30 s flat backoff underpaces a saturated key (SAIA: 2 req/10 s) |
| turn cap | 14 (16 %) | already escalates (ADR, prior change) — no new work |
| bad model / 404 | 17 (19 %) | infra, non-verdict, but **invisible** in `af cost` per-worker trust |

Three concrete, code-pinned defects:

1. **The wrong end of the gate output reaches the retry prompt.**
   `Receipt.error` is "the first line of the attempt's failure reason"
   (`state/spec.md` "Cost receipts"), up to 200 chars. For gate failures the
   useful text is the LAST lines. Observed misleads: `T2-CALRS-SERVICE`
   fails exit 127 but the captured line is `yaml OK ['calendar',
   'calendar-radicale']`; `T2-TILES`/`T3-NOSDESK` capture an EMPTY error
   (`exit 1: ;`); Python tracebacks lose their final exception line. ADR-13
   retry memory (and the operator's `af status`) is only as good as that
   line.

2. **Broken gates burn the whole retry budget.** `T-001`, `T-006`
   (`Syntax error: "(" unexpected` — the gate itself is unparsable),
   `T2-TILES` (exit 127), and the World-Office orbit/cube-orient tasks
   ("✗ GATE FAILED … fehlt") fail BYTE-IDENTICALLY on every attempt. af
   re-pays a full agent each fresh attempt — `T-002` spent 579 s + 404 s +
   920 s across three agents on the same `PARSE-FAIL` before giving up.
   This is the "task-design defect" lesson from dogfood20 (1/10 rejected)
   formalized as an af feature.

3. **Infra failure is invisible per worker.** 400/404/429/timeout receipts
   are correctly non-verdicts, so a worker whose *model name drifted* shows
   full trust in `af cost` with no hint of the 17 wasted attempts behind it.
   The WO loop on legion runs `probe-*.sh` for exactly this reason; af
   should surface it in the report it already prints.

## What Changes

**A. Gate-failure receipts carry the tail (`src/state.rs` receipt write,
`src/execute.rs:157`).** For a failure whose reason starts `acceptance gate
failed`, `Receipt.error` SHALL be the LAST up-to-200 characters of the
gate's combined output (line-boundary aware), with `(gate produced no
output; exit N)` when the output is empty; every other failure keeps the
existing first-line rule. Retry prompts (ADR-13) and `af status` then show
the real reason. `af cost`'s by-reason grouping continues to work (the
`acceptance gate failed (exit N):` prefix is preserved before the tail).

**B. Repeated identical gate failures stop early (`src/run.rs` reap).**
When a task's most recent AGENT-driven attempt (not a gate-only rerun) dies
at the gate with a gate-error fingerprint IDENTICAL to the previous
agent-driven attempt's — same exit code and whitespace-normalized output
tail — the task SHALL be marked `failed` immediately (attempts <
`max_attempts` is allowed) with reason `repeated identical gate failure
(<fingerprint>) — task design defect`. A differing fingerprint (flake or
partial progress) never triggers. This converts e.g. `T-002`'s three agents
into one agent + one cheap gate rerun, and stops a WO batch from spending
five attempts on a gate nobody can pass.

**C. Adaptive per-worker backoff on infra failures (`src/run.rs:104`).**
The flat 30 s cooldown becomes escalating: 30 s → 60 s → 120 s → 240 s →
300 s (cap) per worker on repeated infra-class failures (429, 5xx, timeout,
transport), reset on any verdict (merged/verified/non-infra failed). Backed
off workers stay unpickable via the existing `pick_worker` filter. When the
pool has exactly ONE enabled worker the cooldown stays at 30 s (a
5-minute lockout of the only worker helps nobody).

**D. Worker infra-drift visible in `af cost` (`src/run.rs` cost report).**
The per-worker trust table SHALL gain an `INFRA` column — the count of
infra-class failed receipts in the selected window — and a footnote naming,
per worker, the most recent infra failure's reason with its class. A worker
with 3 dead-model 400s + 2 merged now shows `2/2` trust AND `INFRA=3` with
the reason, so a silent drift is one glance instead of buried in `WASTED`.
Same window/narrowing rules (`--last`, `--since`, `--task`) as the rest of
the report.

## Non-Goals

- **No per-task opt-out for B in v1.** The trigger is deliberately
  conservative (agent-driven attempts only, byte-identical fingerprints). A
  `retry_identical` flag is a follow-up only if a false positive is
  observed in the field.
- **No change to verdict math.** Infra failures stay non-verdicts (ADR-9 /
  r9); only their *visibility* changes (A, D) and the *pacing* (C).
- **No retry-budget change.** B stops EARLY; it never extends
  `max_attempts` or overrides the turn-cap escalation.
- **No gate-runner changes.** Gates stay user-authored shell; `gate.rs`
  only supplies output the *receipt* uses differently.
- **No probe/network feature** (ADR-1): pre-flight model checks stay the
  operator-side `scripts/wo-loop/probe-*.sh`.
