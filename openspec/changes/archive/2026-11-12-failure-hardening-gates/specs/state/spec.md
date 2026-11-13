# Capability: state

## MODIFIED Requirements

### Requirement: Cost receipts

Each attempt SHALL append a receipt (task id, attempt number, worker, model, wall-clock elapsed, agent-reported tokens if available) to `state/receipts/` — every attempt end, including failed attempts (wall-clock truth, ADR-9). Receipts carry `outcome` (`"merged"` | `"failed"` | `"interrupted"` | `"recovered"`) and `error: Option<String>` — the first line of the attempt's failure reason, capped at 200 characters, `None` for merged attempts; EXCEPT for a failure whose reason begins `acceptance gate failed`, whose `error` SHALL be the LAST up to 200 characters of the gate's combined output (boundary-aware, whole lines), or the marker `(gate produced no output; exit N)` when the gate wrote nothing — so retry prompts and `af status` show the real reason at the END of the gate run, not its first banner line. Receipts written before this change SHALL deserialize with outcome `"merged"` and `error = None`. Receipts also carry `cost_micros` — the provider's OWN reported cost for the attempt, as integer MICRO-USD (so the ledger carries no float drift), recorded ONLY when the transcript's `usage.cost.total` is finite and STRICTLY POSITIVE: a reported `0` means "not tracked" (every provider tested here reports exactly 0, indistinguishable from a genuinely free run) and is recorded as `None`, never as a measured free run; a missing `cost`, a `cost` that is not an object, or a `total` that is a string, negative, or NaN is `None` as well. Receipts written before this change SHALL deserialize with `cost_micros = None`. `af cost` SHALL aggregate receipts (last run, since date, or per task).

When startup heal finds a stale `running` attempt whose durable agent
outcome cannot be resumed (it must re-run the agent — phase `Spawned`, a
legacy state file with no phase, or missing resume artifacts), `af` SHALL
append an `interrupted` receipt for that lost attempt. Dispatch SHALL
persist the attempt's identity in the task state in the same save that
marks the task `running` — BEFORE the agent spawns — as two optional
fields: `attempt_started_ts` (the unix timestamp the running attempt was
dispatched) and `attempt_worker` (the worker it was dispatched to), so a
killed orchestrator still leaves behind who was running and when the
attempt started. When that identity is present, the healed receipt SHALL
name the persisted worker, name the configured worker's model (`unknown`
when that worker is no longer in the config — a healed attempt must never
claim a model nobody recorded), carry `wall_clock_s` = time since dispatch
— an UPPER BOUND, because the exit time is unknowable — and an `error`
saying the duration is the time since dispatch and an upper bound. When
the identity is absent (a state file from before this change, or a crash
before the first dispatch save), the receipt keeps the placeholder
behaviour: worker `unknown`, model `unknown`, `wall_clock_s` 0.0, and an
`error` explaining that the orchestrator exited mid-attempt and the
duration is unknown. Either way `interrupted` is NOT a verdict on the
worker: `Receipt::counts_as_verdict` SHALL be false for it, every trust
statistic SHALL exclude it from both the numerator and the denominator,
and the cost report SHALL NOT footnote the now-named worker as missing
from the config. A resume that finishes from durable state (phase
`AgentDone` or `GatePassed`) is not lost and records no `interrupted`
receipt.

#### Scenario: receipt appended per attempt

WHEN an attempt finishes
THEN a receipt record exists for it.

#### Scenario: cost aggregates receipts

WHEN receipts exist for a run
THEN `af cost --last` prints total elapsed and per-task breakdown.

#### Scenario: failed attempts are recorded

GIVEN a task whose agent exits non-zero twice, then merges on attempt 3
WHEN receipts are loaded
THEN there are 3 receipts for the task with outcomes
`failed, failed, merged`.

#### Scenario: legacy receipts stay loadable

GIVEN a receipts file from before this change (no `outcome` field)
WHEN receipts are loaded
THEN outcome is `"merged"` and `af cost` output is unchanged.

#### Scenario: failed receipt carries the reason

GIVEN an agent that exits 7
WHEN the attempt's receipt is loaded
THEN `error` contains the failure's first line.

#### Scenario: legacy receipts parse with no error

GIVEN a receipt file from before this change
WHEN receipts are loaded
THEN `error` is `None`.

#### Scenario: A reported cost is captured and a zero is not

GIVEN an agent whose JSON transcript reports a positive `usage.cost.total` (USD) on its final usage event
WHEN the attempt's receipt is loaded
THEN `cost_micros` equals that amount in micro-USD and the task log's usage line appends it as `($0.0123)` with four decimals; and a transcript whose `cost.total` is 0 records `cost_micros` `None` with the usage line unchanged — a reported zero is not a measurement, so it is never recorded as a free run.

#### Scenario: interrupted attempts are recorded as non-verdicts

GIVEN a task left `running` by a killed orchestrator with no durable resume artifacts
WHEN the next `af run` heals the stale attempt by re-running the agent
THEN an `interrupted` receipt naming the task and attempt exists with `wall_clock_s` 0.0 and an `error` explaining the duration is unknown, and `Receipt::counts_as_verdict` is false for it.

#### Scenario: a healed receipt names the worker and an upper-bound duration

GIVEN a task left `running` by a killed orchestrator whose persisted state carries `attempt_worker` and `attempt_started_ts`
WHEN the next `af run` heals the stale attempt by re-running the agent
THEN the `interrupted` receipt names that worker and its configured model, carries a `wall_clock_s` at least the time since dispatch with an `error` saying the duration is an upper bound, and still does not enter any `WINS/TOTAL`.

#### Scenario: a state without attempt identity heals with the placeholder

GIVEN a task left `running` by a killed orchestrator whose persisted state carries neither `attempt_worker` nor `attempt_started_ts`
WHEN the next `af run` heals the stale attempt by re-running the agent
THEN the `interrupted` receipt records worker `unknown` and `wall_clock_s` 0.0 with an `error` explaining the duration is unknown.

#### Scenario: gate-failure receipts carry the failing tail

GIVEN an agent whose change fails the acceptance gate (the gate's combined output ends in `YAML-FAIL k8s/openedusuite/deploy-drawio.yaml` after a long banner)
WHEN the failed attempt's receipt is loaded
THEN `error` starts with `acceptance gate failed (exit 1):` and ends with the gate output's last line `YAML-FAIL k8s/openedusuite/deploy-drawio.yaml`, not the banner's first line.

#### Scenario: silent gate failure names the missing output

GIVEN a gate that exits non-zero having printed nothing
WHEN the failed attempt's receipt is loaded
THEN `error` is `acceptance gate failed (exit N): (gate produced no output; exit N)`.

