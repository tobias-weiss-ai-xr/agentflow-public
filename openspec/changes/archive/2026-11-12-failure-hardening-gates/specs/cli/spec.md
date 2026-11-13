# Capability: cli

## ADDED Requirements

### Requirement: Worker infra-drift breakdown in the cost report

`af cost` SHALL show, for each worker in its per-worker table, the infra-failure count for that worker: the number of SELECTED failed receipts whose `failure_class` is `"infra"` (provider 4xx/5xx, rate limit, transport/harness timeout, dead model name). It SHALL render as an `INFRA` column immediately after `MEAN_S`, showing `0`-hidden as `-`, and SHALL NOT alter the verdict-derived `WINS/TOTAL` or trust rate (infra failures stay outside the verdict numerator and denominator). When any worker in the selection has `infra > 0`, `af cost` SHALL append a footnote line naming, per affected worker, the count and the most recent such receipt's `error` (capped 200 chars, one per worker, sorted by worker name, no truncation mid-word); when no selected receipt is infra, the report SHALL be byte-identical to today (no `INFRA` column content and no footnote). The column and footnote SHALL narrow with the same selection as the rest of the report (`--last`, `--since`, `--task`), so a worker's drift over a specific window is visible next to its trust.

#### Scenario: infra failures surface per worker

GIVEN receipts where worker w1 has 3 infra-class failed receipts (dead model 400s) and 2 merged verdicts, and worker w2 has only clean verdicts
WHEN `af cost` runs
THEN the worker table shows `w1` with `2/2` wins/total (`INFRA` = 3) and `w2` with `INFRA` `-`, and a footnote names w1's count and its most recent infra error; `--last` narrows both to the selected receipts.

#### Scenario: an all-healthy report is unchanged

GIVEN receipts where no selected failure is infra-class
WHEN `af cost` runs
THEN no `INFRA` column content and no infra footnote appear (the report matches the pre-change output byte-for-byte).

#### Scenario: worker absence from the config stays a note, not a column

GIVEN a selected infra receipt naming a worker absent from `cfg.workers`
WHEN `af cost` runs
THEN the existing "absent from the config" footnote still names that worker, and the infra breakdown does not invent trust rows for it.
