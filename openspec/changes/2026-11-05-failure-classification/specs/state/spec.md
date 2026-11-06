# Capability: state

## ADDED Requirements

### Requirement: Failure class on receipts

Every `failed` attempt SHALL record a `failure_class` on its receipt:
`"infra"` when the harness, transport, or environment failed (provider
error, agent timeout, stall watchdog, missing binary, kill, git/io error,
or a native-harness failure such as a turn cap) — the worker did not fail,
the infrastructure did — and `"agent"` when the agent's work itself was
determined to be wrong or missing (non-zero exit, acceptance-gate failure,
scope violation, zero committed change, or a merge-verification failure).
`failure_class` SHALL be `None` for non-failed receipts (`merged`/
`verified`), for `interrupted` and `recovered` bookkeeping receipts, and for
legacy receipts written before this field existed (which SHALL deserialize
with `failure_class = None`). Classification SHALL be decided by a single
shared function over the attempt's failure reason, so the persisted receipt
and the live router always agree. `Receipt::counts_as_verdict` SHALL be
false for an `infra`-classified failed receipt, so every trust statistic
excludes it from both the numerator and the denominator — an infra failure
must never lower a worker's trust. An `infra` failure SHALL still count
against the task's `attempts`/`max_attempts` retry budget (the attempt
truly happened) and against cost totals (the tokens were spent); only the
trust statistic ignores it. `af status --json` SHALL surface each task's
most recent `failure_class` (`null` when the task has no failed receipt).

#### Scenario: infra failures are not trust verdicts

GIVEN a worker whose only recorded attempt failed with `failure_class: "infra"`
WHEN trust statistics are computed
THEN the receipt is excluded from both the numerator and the denominator: the worker shows no failed verdict, and the receipt does not lower its trust.

#### Scenario: agent failures remain trust verdicts

GIVEN a worker whose only recorded attempt failed with `failure_class: "agent"`
WHEN trust statistics are computed
THEN the receipt counts as a verdict exactly as before and lowers the worker's trust.

#### Scenario: provider-signal reasons classify as infra

WHEN a failed attempt's reason names a harness, timeout, stall, missing binary, kill, or cannot/connection/provider signal
THEN the receipt's `failure_class` is `"infra"`.

#### Scenario: work-judgement reasons classify as agent

WHEN a failed attempt's reason names a non-zero agent exit, gate failure, scope violation, zero commits, or merge-verification failure
THEN the receipt's `failure_class` is `"agent"`.

#### Scenario: legacy failed receipts stay verdicts

GIVEN a failed receipt written before `failure_class` existed (deserializes with `failure_class: None`)
WHEN trust statistics are computed
THEN it counts as a verdict exactly as before (backward compatible).

#### Scenario: status json surfaces the failure class

WHEN `af status --json` runs for a task whose latest failed receipt carries a `failure_class`
THEN the task's JSON carries that class, and `null` when the task has no failed receipt.
