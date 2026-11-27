# Capability: state

## MODIFIED Requirements

### Requirement: Failure class on receipts

Every `failed` attempt SHALL record a `failure_class` on its receipt:
`"infra"` when the harness, transport, or environment failed (provider
error, transport abort or connection reset, agent timeout, stall watchdog,
missing binary, kill, git/io error, an agent that produced no output at
all, or a native-harness failure such as a turn cap) — the worker did not
fail, the infrastructure did — and `"agent"` when the agent's work itself
was determined to be wrong or missing (a non-zero exit whose output names no
infra signal, acceptance-gate failure, scope violation, zero committed
change, or a merge-verification failure). `failure_class` SHALL be `None`
for non-failed receipts (`merged`/`verified`), for `interrupted` and
`recovered` bookkeeping receipts, and for legacy receipts written before
this field existed (which SHALL deserialize with `failure_class = None`).

The failure reason SHALL be the agent's OWN diagnosis, not only its exit
wrapper: when a CLI worker exits non-zero WITHOUT committed work, af SHALL
append the agent's diagnosis line to the reason (`agent exited NonZero
(code 1); last output: <line>`, ANSI escapes stripped, the line capped at
160 characters). The diagnosis line is the FIRST non-empty line of the
agent's combined output that names an infra signal, falling back to the
last non-empty line when none does — because CLIs follow the cause
(`No API key found for openai.`, `Request was aborted`) with remediation
hints and a docs path, and the cause, not the pointer, belongs on the
receipt; when the agent wrote nothing
at all, the reason SHALL be `agent produced no output (exit N) after Ts`.
The reason's historical prefix (`agent exited …`) SHALL be byte-identical so
existing matching is unaffected; the suffix is additive. Native-harness
workers already carry their precise cause (`harness: <e>`).

Classification SHALL be decided by a single shared function over the
attempt's failure reason, so the persisted receipt and the live router
always agree. The function SHALL recognise the transport vocabulary agents
emit in practice (`aborted`, `empty response`, `rate limit`, `429`, `503`,
`overloaded`, `socket`, `eof`, `reset by peer`, `dns`, `api key`, `not
found for provider`, `no output`) in addition to the
harness/timeout/stall/missing/kill/cannot/provider/connection signals.
`Receipt::counts_as_verdict` SHALL be false for an `infra`-classified failed
receipt, so every trust statistic excludes it from both the numerator and
the denominator — an infra failure must never lower a worker's trust. An
`infra` failure SHALL still count against the task's
`attempts`/`max_attempts` retry budget (the attempt truly happened) and
against cost totals (the tokens were spent); only the trust statistic
ignores it. `af status --json` SHALL surface each task's most recent
`failure_class` (`null` when the task has no failed receipt).

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

#### Scenario: a CLI transport abort is infra

GIVEN a CLI worker exits with code 1, prints `Request was aborted`, and commits nothing
WHEN the receipt is written
THEN `error` begins `agent exited NonZero (code 1); last output: Request was aborted`
AND `failure_class` is `"infra"`.

#### Scenario: a CLI failure with an ordinary error is the agent's

GIVEN a CLI worker exits with code 1, prints `Error: file not found: foo.rs`, and commits nothing
WHEN the receipt is written
THEN `error` carries `last output: Error: file not found: foo.rs`
AND `failure_class` is `"agent"`.

#### Scenario: a silent CLI death is named

GIVEN a CLI worker exits non-zero, writes nothing, and commits nothing
WHEN the receipt is written
THEN `error` is `agent produced no output (exit N) after Ts`
AND `failure_class` is `"infra"`.

#### Scenario: a CLI crash report with a trailing docs pointer names the cause

GIVEN a CLI worker exits with code 1, prints `No API key found for openai.` followed by remediation hints and a documentation path, and commits nothing
WHEN the receipt is written
THEN `error` carries the cause line (`No API key found for openai.` or `not found for provider`), never the documentation path
AND `failure_class` is `"infra"`.

#### Scenario: legacy failed receipts stay verdicts

GIVEN a failed receipt written before `failure_class` existed (deserializes with `failure_class: None`)
WHEN trust statistics are computed
THEN it counts as a verdict exactly as before (backward compatible).

#### Scenario: status json surfaces the failure class

WHEN `af status --json` runs for a task whose latest failed receipt carries a `failure_class`
THEN the task's JSON carries that class, and `null` when the task has no failed receipt.
