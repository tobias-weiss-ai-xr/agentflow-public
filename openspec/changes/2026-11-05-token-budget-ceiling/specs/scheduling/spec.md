# Capability: scheduling

## ADDED Requirements

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
