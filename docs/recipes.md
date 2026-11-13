# Campaign recipes

Practical `af` configurations for common orchestration goals. Every recipe is
self-contained: a **Goal**, the **Config** (JSON snippets in the shape
`af validate` expects), the **Command** to run, and the **Expected outcome**.
All recipes assume the standard layout from the README quick start
(`config/tasks.json`, `config/workers.json`, `AF_REPO_DIR`, `AF_STATE_DIR`).

## Recipe 1 — Cheapest-first escalation: try a cheap model, fall back to a strong one

**Goal.** Spend the least on routine tasks, but let a hard task escalate to a
stronger model on retry instead of burning attempts on a model that can't do
the job. No extra flags — routing is declared per task.

**Config.** `config/tasks.json` — give the task a `cascade`; entries are
ordered cheapest-first and are indexed by attempt (attempt 1 = `cascade[0]`,
attempt 2 = `cascade[1]`, …):

```json
{
  "_meta": { "project": "dogfood" },
  "tasks": [
    {
      "id": "fix-typo",
      "title": "Fix the typo in README",
      "deps": [],
      "scope": ["README.md"],
      "accept": "grep -q 'the fix' README.md",
      "acceptance_prose": "Typo is gone from README.",
      "manual": false,
      "cascade": ["cheap", "pro"]
    }
  ]
}
```

`config/workers.json` must contain both cascade members (`af validate`
rejects unknown, duplicated, or empty cascade entries):

```json
{
  "workers": [
    { "name": "cheap", "provider": "openai",    "model": "gpt-4o-mini", "api_key_env": "OPENAI_API_KEY",    "enabled": true, "cli": "pi" },
    { "name": "pro",   "provider": "anthropic", "model": "claude-opus-4", "api_key_env": "ANTHROPIC_API_KEY", "enabled": true, "cli": "pi" }
  ]
}
```

**Command.**

```sh
af validate
af run
```

**Expected outcome.** Attempt 1 is dispatched to `cheap`. If the gate passes,
the branch merges and `pro` is never paid. If attempt 1 fails, attempt 2
escalates to `pro`. Past the end of the cascade (here: attempt 3) the router
returns to normal UCB1 selection. An explicit `--worker NAME` or `--task`
pin overrides the cascade, and a busy or disabled cascade member also falls
back to UCB1 — a cascade only changes *who* is dispatched, never *whether*
the attempt is gated or merged.

## Recipe 2 — Recovering paid-for work: land a failed attempt without re-invoking an agent

**Goal.** A failed attempt (timeout, gate miss, scope violation, merge
conflict) already cost money. Its branch is archived — never deleted — as
`<prefix>/<id>-rejected-<attempt>-<ts>`; land it manually with `af recover`,
or let the next `af run` reuse it for free.

**Config.** Nothing special is required, but make the failure recoverable by
keeping the scope accurate. If the original failure was a scope violation,
widen the scope in `config/tasks.json` — recovery re-checks the *current*
scope and gate:

```json
{
  "tasks": [
    {
      "id": "fix-typo",
      "title": "Fix the typo in README",
      "deps": [],
      "scope": ["README.md", "docs/faq.md"],
      "accept": "grep -q 'the fix' README.md",
      "acceptance_prose": "Typo is gone from README.",
      "manual": false
    }
  ]
}
```

**Command.** Inspect first, then land:

```sh
af recover --task fix-typo --dry-run   # shows what WOULD happen, touches nothing
af recover --task fix-typo             # newest archive; add --attempt N to pin one
```

**Expected outcome.** `af recover` finds the newest archive for the task
(searched as `<prefix>/<id>-rejected-<ts>`, or
`<prefix>/<id>-rejected-<attempt>-<ts>` with `--attempt N`; exit 2 with
nothing to recover), checks the archived branch out in a throwaway worktree,
re-runs the scope check and the acceptance gate against the *current*
configuration, and merges on pass — the agent is **not** re-invoked, so the
work costs nothing extra. Independently, `af run` scans qualifying archives
before dispatch and reuses one automatically when it already satisfies the
task's scope and gate; set `AF_NO_REUSE=1` to disable that pre-dispatch
reuse and always pay for a fresh attempt.

## Recipe 3 — Multi-repo campaigns: one campaign across several repositories

**Goal.** Orchestrate work that spans more than one repository while keeping
branches, worktrees and merges inside each task's own repo. Dependencies
then order work across repos automatically.

**Config.** `config/repos.json` (or pass `--repos`; `AF_REPOS_JSON` also
works) maps logical names to absolute paths. Paths resolve relative to the
repos.json file's directory; the name `main` (or an omitted `repo` field)
means the default repo `AF_REPO_DIR`, and any other name must appear in the
map:

```json
{
  "repos": {
    "main": "/abs/path/to/agentflow",
    "docs": "/abs/path/to/docs-site"
  }
}
```

`config/tasks.json` — each task targets one repo via its `repo` field, and
`deps` order work across repos:

```json
{
  "_meta": { "project": "dogfood" },
  "tasks": [
    {
      "id": "fix-readme",
      "title": "Document the --repos flag in the README",
      "repo": "main",
      "deps": [],
      "scope": ["README.md"],
      "accept": "grep -q -- '--repos' README.md",
      "acceptance_prose": "README documents the --repos flag.",
      "manual": false
    },
    {
      "id": "fix-docs-index",
      "title": "Refresh the docs site index page",
      "repo": "docs",
      "deps": ["fix-readme"],
      "scope": ["index.md"],
      "accept": "git diff --stat --exit-code",
      "acceptance_prose": "Docs index is updated.",
      "manual": false
    }
  ]
}
```

**Command.**

```sh
af validate --tasks config/tasks.json --workers config/workers.json --repos config/repos.json
af run      --tasks config/tasks.json --workers config/workers.json --repos config/repos.json
```

**Expected outcome.** `fix-readme` creates its worktree and branch in the
`main` repo and merges there; `fix-docs-index` runs only after `fix-readme`
is `done` and lands in the `docs` repo. Each attempt, gate run and merge is
scoped to its task's own repo; a `repo` name missing from `repos.json`
warns and falls back to the default repo rather than failing the campaign.

## Recipe 4 — Sandboxing agents: run untrusted CLIs under a syscall/network jail with a minimal env

**Goal.** Contain the agent process: prepend a sandbox command to its argv
and expose only the credentials it needs, so a prompt-injected agent cannot
reach the network or read the orchestrator's environment.

**Config.** `config/workers.json` — `api_key_env` names the single secret
var each worker may inherit:

```json
{
  "workers": [
    { "name": "pro", "provider": "anthropic", "model": "claude-opus-4", "api_base": "https://api.anthropic.com/v1", "api_key_env": "ANTHROPIC_API_KEY", "enabled": true, "cli": "pi" }
  ]
}
```

**Command.** Export the wrapper and the env allowlist before `af run`:

```sh
export AF_SANDBOX_CMD="firejail --net=none"
export AF_AGENT_ENV_PASSTHROUGH="HTTP_PROXY,HTTPS_PROXY"
af run
```

**Expected outcome.** Every agent child is spawned as
`firejail --net=none <cli> --provider … --model … -p @<prompt-file>` inside
its worktree, with no network access. The child's environment is
allow-listed (ADR-10): it gets only its worker's `api_key_env` variable,
the vars named in `AF_AGENT_ENV_PASSTHROUGH` (comma-separated), and the
built-in git hygiene pairs (`GIT_TERMINAL_PROMPT=0` plus a cleared
`credential.helper`) — so agents can never be prompted for or exfiltrate
other secrets. Leave `AF_SANDBOX_CMD` empty (the default) for no wrapper;
the env allowlist applies either way.

## Pacing a campaign with budgets

**Goal.** Keep a batch campaign inside a fixed spend envelope: a wall-clock
ceiling and a token ceiling, so a runaway loop cannot bill a surprise. Both
ceilings are compared against the same receipts `af cost` reports, so the
number the orchestrator enforces is the number the operator sees.

**Config.** The campaign caps are environment variables — nothing in
`tasks.json`/`workers.json` has to change:

```sh
export AF_MAX_WALL_CLOCK_S=7200    # summed attempt wall-clock, seconds
export AF_MAX_TOKENS=2000000       # summed attempt tokens (integer)
```

`AF_MAX_WALL_CLOCK_S` is compared against the sum of `wall_clock_s` over the
receipts of the in-scope tasks — the exact TOTAL `af cost` prints — and
`AF_MAX_TOKENS` against the same receipts' `TOKENS` column. Receipts are
persisted, so a ceiling counts across restarts and resumes; `0` (the default
for both) disables a cap. A task may also declare its own `max_tokens`, which
composes with the campaign cap and `max_attempts`:

```json
{
  "tasks": [
    {
      "id": "docs-refresh",
      "title": "Refresh the three stale reference pages",
      "deps": [],
      "scope": ["docs/*.md"],
      "accept": "test -s docs/reference.md",
      "acceptance_prose": "Reference pages are refreshed.",
      "manual": false,
      "max_tokens": 400000
    }
  ]
}
```

**Command.** Preview for free, then run paced and watch the ledger:

```sh
af validate
af run --dry-run    # prints the plan and the current spend line; dispatches nothing
af run              # the paced campaign
af cost             # the live ledger: TOTAL seconds/tokens, trust, waste
```

**Expected outcome.** `af run --dry-run` prints the execution plan and the
budget line up front — and if a previous campaign's receipts already put the
spend at or over a cap, it says so immediately, creating nothing and exiting
`0`. During the run, hitting either ceiling stops **new** dispatches only:
attempts already in flight are allowed to finish and their receipts land
normally (a running attempt is never killed). `af run` then prints one
diagnostic naming which ceiling bit, the spend, the cap and how many in-scope
tasks were not started, and exits `3` — no state is lost, so raising or
clearing the cap and re-running resumes from atomic state. **Completion
wins**: a campaign whose in-scope tasks are all already `done` still exits `0`
even over the cap. Starting point for a docs-tier campaign on turbo-class
models: `7200` s / `2000000` tokens (2 h / 2 M), then scale with the task
count — `af cost` tells you the real per-task rate to recalibrate with.

## Cheapest-first escalation economics

**Goal.** Know what escalation actually costs before you enable it. Recipe 1
declared the routing; this recipe prices it: attempt 1 rides the cheap model,
retries climb the cascade, and per-worker declared prices turn `af cost` into
a blended bill across rungs — with the guarantee that an infra failure never
punishes the cheap rung's trust.

**Config.** `config/tasks.json` — the cascade is per task: attempt 1 runs
`cascade[0]`, attempt 2 escalates to `cascade[1]`, and past the end (or when a
rung is busy or disabled) routing falls back to UCB1:

```json
{
  "_meta": { "project": "dogfood" },
  "tasks": [
    {
      "id": "expand-faq",
      "title": "Expand the FAQ with the three newest questions",
      "deps": [],
      "scope": ["docs/faq.md"],
      "accept": "grep -q 'How do I' docs/faq.md",
      "acceptance_prose": "FAQ answers the three newest questions.",
      "manual": false,
      "cascade": ["cheap", "pro"]
    }
  ]
}
```

`config/workers.json` — declare what each rung costs so `af cost` can blend
the spend. A declared `price_per_mtok_usd` must be finite and positive, and it
outranks the `params_b` proxy:

```json
{
  "workers": [
    { "name": "cheap", "provider": "openai",    "model": "gpt-4o-mini",   "api_key_env": "OPENAI_API_KEY",    "enabled": true, "cli": "pi", "price_per_mtok_usd": 0.60 },
    { "name": "pro",   "provider": "anthropic", "model": "claude-opus-4", "api_key_env": "ANTHROPIC_API_KEY", "enabled": true, "cli": "pi", "price_per_mtok_usd": 15.00 }
  ]
}
```

**Command.**

```sh
af validate
af run                          # attempt 1 on "cheap", retries escalate to "pro"
af cost                         # blended spend + per-worker trust across the cascade
af cost --task expand-faq       # the same ledger narrowed to one task
af status --json                # each task's latest failure_class, null when none
```

**Expected outcome.** Attempt 1 is dispatched to `cheap`; if its gate passes,
`pro` is never paid and the COST column reflects the cheap price alone. If
attempt 1 fails, attempt 2 escalates to `pro`, and with both rungs priced the
`af cost` table shows the blended spend over the selected receipts — derived
from the declared prices whenever the provider reports no price (a
provider-reported `cost_micros` measurement outranks the declaration for the
attempt it belongs to). Escalating on infra errors is **safe**: every failed
receipt records a `failure_class`, and `infra` failures — provider errors
like 429s, transport/timeout deaths, the stall watchdog, a missing binary —
are not verdicts: they never enter `WINS` or `TOTAL`, so a `cheap` rung hit by
an outage is not demoted and is routed to again at full trust next campaign.
Only an `agent` failure (gate miss, scope violation, zero commits) lowers
trust. The escalation still costs you the retry: an `infra` failure burns one
`max_attempts` attempt and its tokens still count in `af cost` — which is
exactly why the prices above are worth declaring.
