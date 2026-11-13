# Campaign post-mortem template

Fill in one copy of this file per `af run` campaign and commit it beside the
campaign's `config/tasks.json` / `config/workers.json`. Every number comes
from one command — run `af cost` against the campaign's state dir and paste
what it prints; do not estimate from memory. The underlying data is the
receipts: state receipts live under `<state>/receipts/`, one JSON per
attempt, with fields `task`, `attempt`, `worker`, `model`, `wall_clock_s`,
`tokens`, `ts`, `outcome`, `error`, `failure_class`, and (when reported)
`cost_micros`. Cite receipt fields verbatim; a post-mortem claim without a
receipt is an anecdote.

## 1. Campaign

- Name: <short human name>
- Repo: <path or URL>
- Base commit: <sha the campaign started from>
- Started: <date/time> — Finished: <date/time>
- Orchestrator: <`af --version` output>

## 2. Configuration

- Tasks declared: <N> (`config/tasks.json`)
- Workers declared: <N> (`config/workers.json`)
- max_parallel: <N> (`AF_MAX_PARALLEL`)
- Retry budget: `defaults.max_attempts` = <N>
- Timeouts: `agent_timeout_s` = <N>s, stall watchdog = <N>s (0 = disabled)
- Cost basis in force: <declared prices / `params_b` proxy / provider-reported>

## 3. Outcome table

Copy from `af cost` (columns TASK / ATTEMPTS / WALL_S / TOKENS / COST /
MODEL). STATE is the last attempt's receipt `outcome`: `merged`, `verified`,
`failed`, `interrupted`, or `recovered`. Tokens shows the receipt `tokens`
total (`n/a` where the provider does not report usage).

| Task         | State  | Attempts | wall_s | Tokens | Notes                    |
|--------------|--------|----------|--------|--------|--------------------------|
| docs-readme  | merged | 1        | 182.4  | 41230  | gate passed first try    |
| <task-id>    | <state>| <n>      | <s>    | <n>    | <why, if not merged>     |

Worked example (row 1 above): task `docs-readme` merged on attempt 1, one
receipt at `<state>/receipts/` with `wall_clock_s: 182.4`, `tokens: 41230`.

## 4. Incidents

One block per incident — anything that cost an attempt, a stall, or an
operator intervention. Quote the proving receipt's fields rather than
paraphrasing them.

### Incident <n>: <one-line title>

- Symptom: <what was observed — the `af` output line, gate failure, stall>
- Root cause: <why it actually happened>
- Intervention: <mid-campaign change: scope fix, `af recover`, config edit,
  orchestrator restart>
- Cost (receipt fields): worker=`<id>`, model=`<id>`, wall_clock_s=`<s>`,
  tokens=`<n>`, failure_class=`<infra|agent|—>` , outcome=`<state>`,
  attempt=`<n>`
- Receipt: `<state>/receipts/<file>.json`

Worked example: worker=`glm`, model=`glm-4.6`, wall_clock_s=`316.8`,
tokens=`n/a`, failure_class=`agent`, outcome=`failed`, attempt=`1` — gate
rejected an out-of-scope edit; scope widened, `af recover` merged the
archived branch.

## 5. Recovery analysis

`af cost` prints `RECOVERED: N attempt(s) — Xs of failed work reclaimed (not
wasted)` when archived branches were salvaged by automatic reuse or
`af recover` instead of re-running the agent. The paired failed receipt
leaves `WASTED`; its wall-clock is work that was paid for once and landed.

- RECOVERED line (paste): `RECOVERED: <n> attempt(s) — <x>s of failed work reclaimed`
- WASTED line (paste): `WASTED: <x>s on <n> of <m> attempt(s) (<p>%)`
- Wasted time by failure class: `infra` = <x>s on <n> attempt(s) (harness,
  transport, environment — not a verdict on the worker); `agent` = <y>s on
  <m> attempt(s) (wrong work). Refine with `WASTED BY REASON` when one cause
  dominates.
- Verdict: <did recovery pay off? what would the re-runs have cost?>

## 6. Lessons

- Keep: <what worked — repeat next campaign>
- Drop: <what spent wall-clock and produced nothing>
- Change: <one concrete config or process change, with the field to edit>

## 7. Follow-ups

| Ref              | One-line description             | Filed as          |
|------------------|----------------------------------|-------------------|
| <issue/task id>  | <what must change before next run> | <link or ref>   |
