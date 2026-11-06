# scheduling Specification

## Purpose
TBD - created by archiving change rust-orchestrator. Update Purpose after archive.

## Requirements

### Requirement: Dependency DAG

The scheduler SHALL build a DAG from task `deps`, and SHALL only dispatch a task when all of its dependencies are in state `done`. The DAG SHALL be validated for cycles at load time.

#### Scenario: dependency ordering

WHEN task B lists A as a dep and A is not yet done
THEN B is not dispatched, and is dispatched only after A reaches `done`.

#### Scenario: cycle rejected

WHEN tasks form a dependency cycle
THEN loading fails with an error naming the cycle.

### Requirement: Critical-path priority

Tasks with a deeper dependency depth SHALL be dispatched before shallower tasks when both are ready.

#### Scenario: deeper task first

WHEN A and B are both ready and B has dependents while A has none
THEN B is dispatched before A.

#### Scenario: priority breaks ties among equally deep ready tasks

GIVEN several ready tasks at the same dependency depth with different `priority` ranks and no scope overlap
WHEN the scheduler orders them for dispatch
THEN the higher rank is dispatched first, and tasks with equal rank keep their original order from the config file.

### Requirement: Scope contention avoidance

When two ready tasks have overlapping `scope` file globs, they SHALL NOT be dispatched concurrently (default `defer`: one waits for the other to finish).

#### Scenario: overlapping scope deferred

WHEN task X and task Y both declare the same file glob and both are ready
THEN only one is dispatched; the other is held until the first finishes.

#### Scenario: disjoint scope dispatches in parallel

WHEN task X and task Y declare disjoint globs
THEN both may be dispatched concurrently.

### Requirement: Deadlock detection

When the scheduler has ready-queueable work only through tasks that are permanently `failed`, or when no task can make progress, `af run` SHALL exit cleanly with a non-zero status and a message naming the blocking tasks.

#### Scenario: all tasks blocked by failure

WHEN every remaining task depends on a task that is `failed` with attempts exhausted
THEN the run exits with status code 2 and lists the blocked tasks.

#### Scenario: absent dependency is a deadlock

WHEN a task's dep id does not exist in the loaded config (merged in from a sibling file that is not present)
THEN the run exits cleanly with status 2 instead of looping forever.

#### Scenario: no deadlock while progress possible

WHEN at least one task is `running` or `ready`
THEN the run does not exit.

### Requirement: UCB1 worker selection

Among free, enabled workers eligible for dispatch, `af` SHALL pick the
worker maximizing `mean_reward + sqrt(2·ln(N+1)/(n+1))` where `n` is the
worker's recorded attempts, `N` the total recorded attempts (replayed from
receipts at startup, updated in-memory per attempt), and reward is 1 for a
merged attempt, 0 for a failed one. The trust mean SHALL be Laplace-smoothed
`(wins+1)/(attempts+2)` rather than the raw `wins/attempts`: a single attempt
is 0-or-1 noise, so a lone failure scores 1/3 (not 0) and a lone win 2/3
(not 1), converging to the empirical rate as attempts accumulate; the display
trust column in `af cost` SHALL keep showing the RAW rate — measured data
stays measured, the smoothed mean is the routing term only. A strictly higher
score SHALL always win: measured reliability never yields to a declared
expense assumption — cost-per-task is confounded by task difficulty (the hard
work goes to the trusted worker), so a cost term in the score itself would
penalise a worker for being given the hard tasks and starve it. A score tie
within a small tolerance SHALL prefer the worker with the strictly cheaper
DECLARED cost basis (`params_b` / `price_per_mtok_usd`) when the two bases
are comparable; when the costs do not decide — bases incomparable or equal —
the tie SHALL go to the worker with the strictly lower MEAN wall-clock
duration over its VERDICT attempts (`merged` or `failed`; an `interrupted`
receipt's duration is a placeholder, never a measurement, and SHALL NOT enter
the mean); remaining ties SHALL break in config order, so a fresh state with
no declared bases reproduces the previous first-free behavior exactly. The
full tie-break ordering is therefore trust > declared price > duration >
config order. Duration, like cost, is a tie-break ONLY and never a score
term: wall-clock is confounded by task difficulty (the hard tasks go to
the trusted worker), so a duration term in the score itself would
penalise a worker for being given the hard tasks and starve it — while
UCB1's exploration term still guarantees an unpicked worker is eventually
tried, so no tie-break can starve one. A worker with no duration history
SHALL never displace the incumbent on a tie, and a missing duration SHALL
never be treated as zero.

#### Scenario: fresh state picks first configured worker

GIVEN no receipts exist and all workers are free
WHEN a task dispatches
THEN the first enabled worker in config order is chosen (all score the prior
mean 0.5, an exact tie).

#### Scenario: unexplored worker is tried before a failing one

GIVEN worker A has 0/3 wins and worker B has no history
WHEN both are free
THEN B is chosen (exploration term dominates).

#### Scenario: reliable worker wins at equal counts

GIVEN worker A has 3/3 wins and worker B has 0/3 wins
WHEN both are free
THEN A is chosen.

#### Scenario: a single loss is not scored zero

GIVEN worker A has 0/1 wins and worker B has no history
WHEN both are free and the exploration terms would not override the means
THEN A's routing mean is 1/3, not 0, so a single disaster does not
disqualify a worker outright.

#### Scenario: cheaper worker wins a score tie

GIVEN two free workers with identical recorded stats (their scores tie) whose declared cost bases are comparable, and the cheaper one is not first in config order
WHEN both are free
THEN the cheaper worker is chosen.

#### Scenario: a strictly better score beats a cheaper competitor

GIVEN a free worker with a strictly better win rate whose declared cost basis is more expensive than its competitor's
WHEN both are free
THEN the better-scoring worker is chosen; cost never overrides measured trust.

#### Scenario: incomparable bases keep config order

GIVEN a score tie where one worker declares a price and the other only a parameter count, or where either worker declares no cost basis at all
WHEN a task dispatches
THEN config order decides; a price is never converted into a parameter count.

#### Scenario: faster worker wins only when trust and cost tie

GIVEN two free workers whose scores tie, whose declared cost bases do not decide the tie (both undeclared, or equal), and whose mean verdict-attempt durations differ
WHEN a task dispatches
THEN the worker with the strictly lower mean duration is chosen regardless of config order, while a strictly better trust score or a strictly cheaper declared price still beats a faster worker; a worker with no duration history never displaces the incumbent (config order stands), and an `interrupted` receipt never enters the mean.

### Requirement: Cascade (escalation) routing

A task SHALL accept an optional `cascade` field: an ordered list of worker
names, CHEAPEST-FIRST. When set, attempt `N` of the task SHALL dispatch on
`cascade[N-1]` (the first attempt runs the cheapest worker, the second
escalates to the next, and so on), bypassing UCB1 selection for that attempt.
The acceptance gate SHALL remain the sole arbiter of work quality — a
cascade only changes WHO is dispatched, never whether work is accepted.
When the attempt number is past the end of the cascade, the named worker is
disabled, or the named worker is busy with another task, `af` SHALL fall
back to UCB1 selection exactly as for a task without a cascade. A task
without a `cascade` SHALL use UCB1 for every attempt (unchanged). Validation
SHALL reject, with a hard error naming the task: a cascade entry that names
no worker in `workers.json`, a duplicated worker name within one cascade
(escalation would repeat it), and an empty cascade array (treat as absent
instead). A cascade whose first entry names a disabled worker SHALL warn
non-fatally that every attempt will fall back to UCB1.

#### Scenario: first attempt runs the cheapest cascade worker

GIVEN a task with `cascade: ["cheap", "pro"]` and no failure history
WHEN attempt 1 dispatches
THEN it runs on `cheap` instead of a UCB1 pick.

#### Scenario: a failed attempt escalates to the next worker

GIVEN a task with `cascade: ["cheap", "pro"]` whose attempt 1 failed
WHEN attempt 2 dispatches
THEN it runs on `pro` (the attempt number indexes the cascade).

#### Scenario: past the cascade the router picks again

GIVEN a task with `cascade: ["cheap", "pro"]` whose first two attempts failed
WHEN attempt 3 dispatches
THEN UCB1 selection applies (no cascade entry remains).

#### Scenario: a busy or disabled cascade worker falls back to UCB1

GIVEN attempt `N` names cascade worker `W` and `W` is busy or disabled
WHEN the attempt dispatches
THEN it is not forced to wait for `W` — UCB1 picks among the free, enabled workers.

#### Scenario: a task without a cascade keeps UCB1

GIVEN a task with no `cascade` field
WHEN any attempt dispatches
THEN UCB1 selection applies for every attempt.

#### Scenario: an explicit worker pin overrides the cascade

GIVEN a task with a `cascade` and an explicit `--worker` filter on the run
WHEN any attempt dispatches
THEN the pinned worker is used (the operator's constraint wins over the task's escalation intent).

### Requirement: Scope enforcement on agent edits

After the agent exits successfully, `af` SHALL compute the attempt branch's
changed files (a three-dot diff against the base branch) and check every path
against the task's declared `scope`, using the same matcher as the scheduler's
contention check. A path that no scope entry allows SHALL fail the attempt
before the durable `AgentDone` boundary and before any merge. An empty scope
SHALL allow any file.

#### Scenario: out-of-scope edit fails before merge

WHEN the agent edits a file that no declared scope entry allows
THEN the attempt fails naming the offending path, the task is not merged, and the change never reaches the base branch.

#### Scenario: in-scope edit still merges

WHEN every changed file matches a declared scope entry
THEN scope enforcement passes and the attempt proceeds to the gate and merge.

#### Scenario: empty scope allows any file

WHEN a task declares no scope
THEN every changed file is accepted.

### Requirement: Retry reuses verified agent work

A task SHALL allow up to `max_attempts` attempts (default from worker
defaults). When an attempt fails only at the acceptance gate, the attempt
branch SHALL be kept (it carries the agent's committed, scope-clean work),
and the next attempt SHALL attach to that branch and re-run ONLY the gate —
the agent SHALL NOT be re-invoked while its verified work survives. A scope
violation, agent failure, or merge failure SHALL discard the attempt branch,
and the next attempt SHALL be a fresh agent run on a newly created branch.
If the gate fails again on the gate-only reuse path, the branch SHALL be
dropped so the following attempt is a fresh agent run.

#### Scenario: gate failure retries only the gate

GIVEN a task whose attempt 1 commits its agent work, passes the scope check, and fails only the gate
WHEN the task is retried while the attempt branch still carries that committed work
THEN the retry re-runs only the gate, the agent is not invoked again, and the task merges once the gate passes.

#### Scenario: gate failure keeps the verified branch

GIVEN a task whose agent committed its work and the gate failed
WHEN the failed attempt is cleaned up
THEN only the worktree directory is removed — the attempt branch survives carrying the agent's commits beyond the base branch.

#### Scenario: a second gate failure falls back to a fresh attempt

GIVEN a gate-only retry whose gate fails again with `max_attempts = 3`
WHEN the next attempt starts
THEN the task runs a total of 3 attempts with receipts `failed, failed, merged` — the second gate failure drops the branch so the final attempt is a fresh agent run that can merge.

#### Scenario: attempts are bounded by max_attempts

GIVEN a task whose agent fails on every attempt and `max_attempts = 2`
WHEN the task runs to exhaustion
THEN exactly 2 attempts are made and the task ends `failed`.

### Requirement: Token budget ceiling

`af` SHALL enforce a campaign-level token ceiling and a per-task token
ceiling, both measured from the `tokens` field on the receipt ledger (the
same field `af cost` reports). The campaign ceiling is `AF_MAX_TOKENS`
(`0` = unlimited, the default): once the receipts for the in-scope tasks
total at least this many tokens, NO new attempt SHALL be dispatched
(in-flight attempts still finish) and the run SHALL exit 3 with a
diagnostic naming the spend, the cap, and the count of not-started tasks.
A receipt with `tokens: None` (interrupted, or a CLI worker without token
capture) contributes 0. The campaign ceiling composes with the wall-clock
ceiling: either exhausted ceiling stops new dispatch. The per-task ceiling
is `max_tokens` on a task (`0` or absent = no per-task cap): before each
dispatch, when the task's own receipts already total at least its
`max_tokens`, the task SHALL be set to `failed` with a reason naming the
spend and the cap, and NO further agent SHALL be dispatched for it. Both
ceilings are dispatch gates — they never kill an in-flight attempt.

#### Scenario: campaign token ceiling stops new dispatches

GIVEN a campaign with `AF_MAX_TOKENS=1000` and an in-scope task whose
receipts already total 1200 tokens
WHEN the run evaluates dispatch
THEN no new attempt is dispatched and the run exits 3, leaving the ready
task untouched and the base branch unmerged.

#### Scenario: below the campaign token ceiling dispatch continues

GIVEN a campaign with `AF_MAX_TOKENS=1000` and an in-scope task whose
receipts total 200 tokens
WHEN the run evaluates dispatch
THEN the task is dispatched normally and the campaign completes with exit 0.

#### Scenario: zero campaign token ceiling is unlimited

GIVEN `AF_MAX_TOKENS=0` (the default) and an in-scope task whose receipts
total an arbitrarily large number of tokens
WHEN the run evaluates dispatch
THEN the token ceiling never stops dispatch; the campaign completes with
exit 0.

#### Scenario: receipts without tokens contribute nothing

GIVEN a campaign with `AF_MAX_TOKENS=1000` where the only receipt for an
in-scope task carries `tokens: None` (e.g. an interrupted attempt)
WHEN the run evaluates dispatch
THEN the token ceiling does not trigger; the campaign completes with exit 0.

#### Scenario: per-task token ceiling fails the exhausted task

GIVEN a task declaring `max_tokens: 500` whose own receipts already total
600 tokens
WHEN the task next becomes ready for dispatch
THEN the task is set to `failed` with a reason naming the token spend and
the cap, no agent is dispatched, and nothing is merged.

#### Scenario: per-task token ceiling below the cap dispatches

GIVEN a task declaring `max_tokens: 500` whose own receipts total 400 tokens
WHEN the task becomes ready for dispatch
THEN the task is dispatched normally.

#### Scenario: dry-run surfaces an exhausted token ceiling

GIVEN `AF_MAX_TOKENS=1000` and receipts totaling 1200 tokens
WHEN `af run --dry-run` runs
THEN the printed plan names the token spend and the cap, nothing is
created or changed, and the command exits 0.
