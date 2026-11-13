# lifecycle Specification

## Purpose
TBD - created by archiving change rust-orchestrator. Update Purpose after archive.

## Requirements

### Requirement: Execute pipeline

For each task attempt, `af` SHALL: create a git worktree on a fresh branch; render a prompt from the task template; spawn the agent CLI with `--provider <p> --model <m> -p @<prompt-file>`; if the agent succeeds, run the acceptance gate; if the gate passes, merge the branch to the base branch; otherwise record failure/retry. When an attempt fails after the agent has committed work — a non-zero exit, a stall-watchdog kill, the total agent timeout, a scope violation, an acceptance-gate failure, or a merge conflict — `af` SHALL preserve that committed work on an archived branch (`<branch>-rejected-<now>`) rather than destroying it with cleanup, and SHALL name that branch in the failure reason (so the receipt carries it). Archiving is best-effort and can never fail the attempt. In addition to archiving a copy, when a NATIVE agent failure (turn cap, timeout, provider error) leaves committed work behind, `af` SHALL leave the attempt branch itself durable — dropping only the worktree, never the branch — so the next attempt attaches to that branch as a gate-only retry (`-- gate-only retry on committed agent work --`) and never re-pays the agent. The at-most-one-gate-rerun bound applies: if that gate-only retry fails the gate a second time, `af` SHALL archive the work and drop the branch so the following attempt is a full fresh agent run.

#### Scenario: happy path

WHEN the agent exits 0 and the acceptance gate exits 0
THEN the task branch is merged to the base branch and the task reaches `done`.

#### Scenario: gate failure

WHEN the agent exits 0 but the gate exits non-zero
THEN the task is marked `failed` (or retried), never merged.

#### Scenario: agent failure

WHEN the agent CLI exits non-zero
THEN the attempt fails without running the gate and without merging.

#### Scenario: native failure keeps the attempt branch durable

WHEN a NATIVE agent attempt (turn cap, timeout, or provider error) fails after committing in-scope work, and `preserve_and_note` reports a kept archive, and the gate then passes on the retry
THEN the attempt branch is left durable (only the worktree is dropped), the next attempt attaches as a gate-only retry (`-- gate-only retry on committed agent work --`), the agent is never re-invoked, the archived `-rejected-` copy is still made and survives, and the retried work merges. A second gate failure on that retry archives the work and drops the branch, so the following attempt is a fresh agent run (at most one cheap gate re-run).

#### Scenario: native failure with no committed work cleans up

WHEN a NATIVE agent attempt fails with nothing to keep (`preserve_and_note` reports no archive)
THEN the attempt branch and its worktree are both removed exactly as before the durable-branch behaviour.

### Requirement: Subprocess execution contract

All subprocesses (agent, gate, git) SHALL run through one helper that captures stdout/stderr, applies a hard timeout, kills the process tree on timeout or abort, and classifies the result by exit code (success / non-zero / killed-by-us / missing-binary). No subprocess SHALL outlive the orchestrator.

#### Scenario: timeout kills

WHEN a gate exceeds its timeout
THEN the process is killed, the attempt fails, and no merge happens.

### Requirement: Prompt rendering

A task prompt SHALL be rendered from the configured prompt template — `prompts/worker.md` by default, or the file named by `AF_PROMPT_FILE` — with the task's `title`, `id`, `scope`, `acceptance_prose`, and `context` substituted, and written to a file passed to the agent CLI. The `{{CONTEXT}}` placeholder SHALL be replaced with the task's context (empty when the task declares none); a `{{#CONTEXT}}…{{/CONTEXT}}` conditional section in the template SHALL drop entirely when the task declares no context and render otherwise. For attempt ≥ 2, the rendered prompt SHALL also include a "Previous attempts" block listing this task's earlier failed attempts (attempt number + error line) so the agent avoids repeating them: when the template carries the `{{PREVIOUS_ERROR}}` token the block is substituted into it, otherwise it is appended to the prompt. Attempt 1 SHALL render without the block (the `{{PREVIOUS_ERROR}}` token, if any, renders empty). A configured template that omits `{{CONTEXT}}` for a task that declares context SHALL have the context appended as a "Task context" section (with a warning), so crafted context is never silently dropped. The template file SHALL be re-read for every rendered prompt, so an edited template takes effect on the next attempt without a restart.

#### Scenario: template substitution

WHEN a task with title and acceptance prose is dispatched
THEN the rendered prompt file contains the task title and acceptance prose.

#### Scenario: task context is injected

GIVEN a task declaring `context: "read docs/CONVENTIONS.md first"`
WHEN the prompt renders from a template containing `{{CONTEXT}}`
THEN the rendered prompt contains that exact context in place of the placeholder.

#### Scenario: conditional context section vanishes when unset

GIVEN a template containing `{{#CONTEXT}}Remember:{{CONTEXT}}{{/CONTEXT}}`
WHEN a task without context renders
THEN neither `{{#CONTEXT}}`, `{{CONTEXT}}`, nor the surrounding section text appears in the prompt.

#### Scenario: a task-context template that omits the placeholder is repaired

GIVEN a task with context and a custom template that contains no `{{CONTEXT}}`
WHEN the prompt renders
THEN the context appears as an appended `## Task context` section and a warning names the template.

#### Scenario: retry prompt names the earlier failure

GIVEN attempt 1 of task A failed with an agent exit code
WHEN attempt 2 renders its prompt
THEN the prompt contains a previous-attempts entry for attempt 1.

#### Scenario: first attempt has no history block

GIVEN a task dispatching its first attempt
WHEN the prompt renders
THEN no previous-attempts block appears (and the `{{PREVIOUS_ERROR}}` token, if any, renders empty).

#### Scenario: custom template is selected per campaign

GIVEN `AF_PROMPT_FILE` names a template that is otherwise complete but omits the exact gate command
WHEN a prompt renders
THEN the template is used and the auto-append hardening still adds the missing gate command section.

### Requirement: Gate replay contract

Every task SHALL carry a `gate_replay` flag that defaults to `true`, and `af`
SHALL export the effective decision to the acceptance gate as the environment
variable `AF_GATE_REPLAY` (`1` when replay is on, `0` when a task opts out).
The legacy name `TF_GATE_REPLAY` SHALL still be exported with the same value for
existing gates. This lets a user-authored acceptance gate detect a resumed or
replayed run and stay idempotent.

#### Scenario: gate_replay defaults to true

WHEN a task omits `gate_replay`
THEN the loaded task has `gate_replay == true`.

#### Scenario: explicit opt-out parses

WHEN a task declares `"gate_replay": false`
THEN the loaded task has `gate_replay == false`.

#### Scenario: replay decision reaches the gate

WHEN the acceptance gate runs
THEN `AF_GATE_REPLAY` is exported to the gate process as `1` when replay is on and `0` when it is off, and the legacy `TF_GATE_REPLAY` carries the same value.

### Requirement: Prompt placeholder guarantee

Every agent-prompt render path SHALL emit the task's file scope and the exact
acceptance gate command, and SHALL leak no unresolved `{{...}}` placeholder or
conditional marker. A configured template that omits `{{SCOPE}}`,
`{{ACCEPTANCE}}` or `{{ACCEPT_CMD}}` SHALL be reported on stderr and repaired
by appending the missing sections; a template that cannot be read SHALL fall
back to the built-in default. An empty scope SHALL render as the `*` wildcard.

#### Scenario: every render path carries scope and gate command

WHEN a prompt renders from the built-in default, a complete custom template, or a hostile template that omits the placeholders
THEN the output contains every scope path, the task id and title, and the exact `accept` command verbatim.

#### Scenario: no placeholder leaks

WHEN any render path produces a prompt
THEN the output contains no `{{` token, because substituted placeholders and conditional markers are both removed.

#### Scenario: hostile template is repaired

WHEN a custom template omits `{{SCOPE}}`, `{{ACCEPTANCE}}` and `{{ACCEPT_CMD}}`
THEN `af` warns on stderr and appends auto-filled scope, acceptance and gate-command sections.

#### Scenario: empty scope renders the wildcard

WHEN a task declares no scope entries
THEN the rendered prompt shows `*` for the file scope.

### Requirement: Attempt work is durable before it is judged

Before an attempt is judged, `af` SHALL make the agent's work durable on the
attempt branch. When the agent exits 0 leaving uncommitted changes in its
worktree, `af` SHALL stage and commit those changes on the attempt branch
(without passing `--no-verify`, so the operator's git hooks still run) so the
scope check, the acceptance gate and the merge all judge exactly the same
committed tree. If that commit fails, the attempt SHALL fail with a reason
naming the commit error; a tree that cannot be made durable SHALL NOT be
judged or merged.

#### Scenario: dirty worktree is committed before judging

WHEN the agent exits 0 leaving uncommitted work in its worktree
THEN the work is committed on the attempt branch before the scope check, the gate and the merge, and a passing attempt merges with the change present in the base branch's committed tree.

#### Scenario: an uncommittable tree fails the attempt

WHEN the agent's uncommitted work cannot be committed
THEN the attempt fails with a reason naming the commit error and no merge happens.

### Requirement: An attempt that produces no change is not merged

`af` SHALL count the commits the attempt branch carries beyond the base branch
after the agent's work is made durable and BEFORE the scope check. A count of
zero SHALL fail the attempt with a deterministic reason naming the zero-commit
condition; such an attempt SHALL NOT be reported as merged and its task SHALL
NOT reach `done`. Before reporting an attempt as merged, `af` SHALL verify that
the attempt branch tip is an ancestor of the base branch, and SHALL fail the
attempt naming the discrepancy when it is not.

#### Scenario: zero-commit attempt fails, never merged

WHEN the agent exits 0 but the attempt branch carries zero commits beyond the base branch
THEN the task does not reach `done`, the receipt outcome is not `merged`, and the failure reason names the zero-commit condition.

#### Scenario: a reported merge is verified in the base

WHEN the attempt's merge succeeds
THEN `af` verifies the attempt branch tip is an ancestor of the base branch before reporting the attempt as merged.

### Requirement: Verify tasks

A task with `verify: true` SHALL treat its acceptance gate as the SOLE
deliverable: the task succeeds when the gate exits 0, whether or not the agent
produced any committed change. The zero-commit failure SHALL NOT apply to a
verify task — a verify task may pass having committed nothing. A verify task
SHALL NEVER merge its branch to the base; on success `af` SHALL archive the
branch under `<branch>-rejected-<now>` and SHALL record the receipt outcome as
`verified`. On failure the receipt SHALL record the failure as normal, and the
branch SHALL be archived for retry exactly as a non-verify attempt. An optional
`accept_report.json` in the worktree root (`{"C-M1":"pass", ...}`) SHALL be
parsed best-effort into per-contract verdicts stored on the receipt, surfaced
in `status --json` (the `verify`, `verdict` and `contracts` fields) and seeded
into retry prompts as a `failing contracts:` line in the "Previous attempts"
block; a report that is absent or unparsable SHALL NOT fail the attempt.
The same `accept_report.json` is parsed on the merged path — its verdicts
ride the merged receipt's `contracts` field and surface in `status --json`
(a merge is a success, so there is no retry prompt to seed). Validation SHALL
reject a task that declares `verify` together with `manual` or `readonly`, or
that declares `verify` without an `accept` gate.

#### Scenario: verify task passes and archives

GIVEN a `verify: true` task whose acceptance gate is the sole deliverable
WHEN the gate exits 0, even if the agent committed nothing
THEN the branch is archived as `<branch>-rejected-<now>` and never merged, and the receipt records `outcome: "verified"`.

#### Scenario: verify task's acceptance report rides the receipt sorted by key

GIVEN a `verify: true` task whose acceptance gate exits 0 and writes an `accept_report.json` with a per-contract verdict map
THEN the per-contract verdicts are parsed best-effort onto the `verified` receipt's `contracts` field, sorted by contract key (a `BTreeMap` order), and a missing or unparsable report does not fail the attempt.

#### Scenario: status --json surfaces the verify verdict and contracts

GIVEN a `verify: true` task that reached `done` with `outcome: "verified"`
WHEN `af status --json` is rendered
THEN the task's object carries `verify: true`, `verdict: "verified"`, and the per-contract `contracts` array (sorted by contract key); a `verify: true` task that failed the gate surfaces `verify: true` with `verdict: "failed"` and an empty `contracts` array.

#### Scenario: verify task gate fails

GIVEN a `verify: true` task whose acceptance gate exits non-zero
THEN the receipt records the failure (never `verified`), the branch is archived for retry, and no merge happens.

#### Scenario: a merged task's gate writes an acceptance report

GIVEN a non-verify task whose acceptance gate writes `accept_report.json` and exits 0
WHEN the attempt merges
THEN the per-contract verdicts are parsed best-effort onto the merged receipt's `contracts` field and surfaced in `status --json`, and a missing or unparsable report does not fail the merge.

### Requirement: Pre-dispatch reuse of an archived branch

When a task becomes ready and a worker is about to be picked/dispatched,
`af run` SHALL FIRST look for an archived rejected branch for that task in
its repository (`<branch_prefix>/<id>-rejected-<ts>`, with an optional `-<n>`
collision suffix) and select the NEWEST by PARSING the numeric `<ts>` (then
the `<n>` suffix) and never by git's output order or committer dates. It
SHALL re-validate that branch against the task's CURRENT scope and gate
exactly as `af recover` does, reusing the same enforcement helpers: (a)
SCOPE — every path the archived branch changes relative to the merge base
with the base branch MUST be covered by the task's CURRENT `scope` (an empty
scope means any file); (b) GATE — unless the task is `manual`, the
acceptance gate MUST pass on the checked-out archived branch, including the
task's `gate_replay`. When it qualifies, `af run` SHALL merge it with the
message `af: <id> — <title>`, set the task to `Done` with phase `GatePassed`,
consume (delete) the archived branch, and take NO agent dispatch for it. The
gate remains the SOLE arbiter, so reuse can only ever save money and never
accept work a fresh attempt would have had to redo. When the newest archive
is out of scope or fails the gate, `af run` SHALL leave the archived branch
in place for `af clean`/a later `af recover` and fall through to the normal
agent dispatch, without retrying THAT SAME branch again in the same run, and
SHALL log the reason at most once per candidate archive. A SPECIFIC archive SHALL be
judged (scope + gate) at most once per run, and the task SHALL be reconsidered
for pre-dispatch reuse whenever a NEWER archive appears in the same run —
including an archive produced by a failed attempt in that same run (a native
failure archives its work before cleanup, and the retry dispatch must
re-validate it instead of re-buying the work from a fresh agent). The one
exception is a task whose LAST attempt died at the acceptance gate: that work
was already judged by the gate this run, so its same-run archive SHALL NOT be
re-validated in a loop — pre-dispatch reuse SHALL decline and burn the task's
same-run consideration so the NEXT attempt is a FRESH agent run (the
fresh-attempt fallback is the scheduling spec's business, and is what keeps a
SECOND gate failure from starving). No `last_error` (first dispatch) keeps
today's semantics: a pre-seeded archive may be reused without a failing
attempt. Reuse SHALL
NOT fire for a task with no archive, a task already `Done`, or when
`AF_NO_REUSE=1` is set (the operator's explicit escape hatch for a clean
re-run). Reuse SHALL NOT append
a receipt: the attempt that produced the archive already has one, and reuse
is not a new attempt. The temporary worktree SHALL be removed on every path.

#### Scenario: an in-scope archive is reused without an agent

GIVEN a ready task whose CURRENT scope covers the archived branch's change
and whose CURRENT gate passes on it
WHEN `af run` dispatches the task
THEN the archive is merged, the task reaches `done`, the archived branch is
deleted, and no agent is invoked.

#### Scenario: an out-of-scope archive falls through to the agent

GIVEN a ready task with an archived branch whose change includes a file the
task's CURRENT scope does not cover
WHEN `af run` dispatches the task
THEN the archived branch survives and the run dispatches the agent normally.

#### Scenario: a gate-failing archive falls through to the agent

GIVEN a ready task with an archived branch that fails the task's CURRENT
acceptance gate
WHEN `af run` dispatches the task
THEN the archived branch survives and the run dispatches the agent normally.

#### Scenario: a same-run failed attempts archive is revalidated

GIVEN a task whose first attempt commits in-scope work and then fails natively (turn cap, timeout, provider error) in the SAME run, archiving `tf/<id>-rejected-<attempt>-<ts>`, and the attempt count is below `max_attempts`
WHEN the task next becomes ready in the same run
THEN pre-dispatch reuse re-validates that archive (scope + gate) and merges it — the task reaches `done` with `failed, recovered` receipts and no second agent is dispatched.

#### Scenario: a gate-failed attempt is NOT revalidated (fresh-attempt fallback)

GIVEN a task whose LAST attempt failed at the acceptance gate, archiving `tf/<id>-rejected-<attempt>-<ts>` (a second gate failure on a gate-only retry)
WHEN the task next becomes ready in the same run
THEN pre-dispatch reuse declines (the task's same-run consideration is burned) and the run dispatches a FRESH agent — receipts `failed, failed, merged` with no reuse of the gate-rejected archive, exactly the scheduling spec's fallback.

#### Scenario: AF_NO_REUSE opts out

GIVEN `AF_NO_REUSE=1` and a ready task with a reusable archived branch
WHEN `af run` dispatches the task
THEN the archive is not reused and the agent is dispatched normally.

### Requirement: Write-time scope guard for builtin workers

For attempts driven by the builtin harness (`cli: "builtin"`), a `write` or
`edit` tool call whose target path — resolved against the WORKTREE so
`..`/absolute/drive-relative escapes are rejected by the existing containment
guard first — is not covered by any entry in the task's `scope` SHALL be
rejected at tool-call time, and the rejection SHALL be returned to the model
as the tool result naming the path and the allowed scope, so the model can
correct course instead of burning the attempt. An empty `scope` means "any
file" and SHALL allow every path, exactly as at the end-of-attempt check.
This fail-fast check is a guard, not a substitute: the end-of-attempt scope
check and its archive-preserving failure SHALL remain for writes performed
through the `bash` tool and for CLI workers. Scope coverage SHALL be decided
by the same matcher as enforcement (`scheduler::scope_overlap` via
`execute::scope_violations`), so tool-time admission and attempt-end
enforcement always agree.

#### Scenario: out-of-scope write rejected at call time

GIVEN a builtin attempt with scope `["src/**"]`
WHEN it calls `write` on `README.md`
THEN the tool result is an error naming the path and allowed scope, and the
attempt continues.

#### Scenario: in-scope write allowed

GIVEN a builtin attempt with scope `["src/**"]`
WHEN it calls `write` on `src/lib.rs`
THEN the write succeeds.

### Requirement: Readonly attempt completion

`af` SHALL treat a task with `readonly: true` specially at attempt time: the
builtin harness rejects `write`/`edit` tool calls regardless of scope, and
when the agent stops normally the attempt SHALL be reported done WITHOUT
running an acceptance gate and WITHOUT merging — a readonly task produces no
committed task work by construction. A readonly attempt that leaves committed
or uncommitted work SHALL still preserve it via the standard archive path, and
cleanup SHALL remove the worktree. CLI-worker readonly attempts SHALL behave
the same: a normal stop = done, no gate, no merge.

#### Scenario: readonly attempt completes without gate or merge

GIVEN a `readonly: true` task
WHEN its agent stops normally
THEN the attempt is reported done, no acceptance gate runs, and no branch is
merged to the base.

#### Scenario: readonly write rejected by the harness

GIVEN a readonly builtin attempt
WHEN it calls `write`
THEN the tool result is an error stating the task is read-only.

### Requirement: Gate and agent-tool shell selection

Acceptance gates and the builtin harness's `bash` tool SHALL run via
`/bin/sh -c` on unix. On Windows, `af` SHALL prefer a POSIX `sh` on PATH
(git-bash) — probed once per process and cached — and SHALL fall back to
`cmd /C` only when no `sh` is reachable, so that POSIX-authored gates and
agent commands keep working on Windows hosts that ship git-bash. The
selection SHALL be a single shared decision: the agent's `bash` tool and
the acceptance gate runner MUST use the same shell function, so a dialect
that works for one works for the other.

#### Scenario: bash-idiom gate passes on Windows with git-bash

GIVEN a Windows host with git-bash `sh` on PATH
WHEN a gate uses POSIX idioms (`[ ]`, backticks, single-quoted pipes,
`./prog`)
THEN the gate passes under `sh` (where it previously failed as
"'.' is not recognized").

#### Scenario: no POSIX sh falls back to cmd

GIVEN a Windows host without `sh` on PATH
WHEN a gate runs
THEN the gate runs via `cmd /C`.

### Requirement: Turn-cap escalation on retry

For attempts driven by the builtin harness, before the harness loop starts the
orchestrator SHALL inspect stored receipts of the same task: if any earlier
failed attempt's error matches `harness: turn cap N reached`, the attempt SHALL
run with an effective turn cap of `2 × N_max` where `N_max` is the highest such
`N`, clamped to at most **128**, regardless of the configured base cap. If no
prior turn-cap failure exists, the effective cap SHALL resolve exactly as
before (task override → worker field → `TF_AGENT_MAX_TURNS` → 32).

#### Scenario: Retry after turn-cap death doubles the budget

- **WHEN** attempt 1 of task `A` fails with `harness: turn cap 12 reached`
  and attempt 2 is dispatched via the builtin harness
- **THEN** attempt 2 runs with a turn cap of 24

#### Scenario: Escalation is bounded

- **WHEN** a task's receipts record turn-cap failures at 64 and 128
- **THEN** the next attempt runs with a turn cap of 128 (ceiling), not 256

#### Scenario: Non-cap retries keep the configured cap

- **WHEN** attempt 1 of task `A` fails with a provider timeout and attempt 2
  is dispatched
- **THEN** attempt 2 runs with the cap the configuration resolves to
  (no escalation)

#### Scenario: Escalation survives resume

- **WHEN** a run is killed after attempt 1 died at the turn cap, and a fresh
  `af run` against the same state dir dispatches attempt 2
- **THEN** the receipts persisted by attempt 1 still trigger the doubled cap

### Requirement: Dispatch rounds read the current task config

Each dispatch round of `af run` SHALL be planned against the current
tasks/workers files (mtime-gated hot reload), and a reload failure SHALL
degrade to the last good config with a single warning, never to a campaign
abort. When the operator left the parallel ceiling to the default
(`AF_MAX_PARALLEL` unset or `0`), the pool capacity for a round SHALL be
recomputed from the reloaded config's enabled-worker count, so adding or
disabling workers while the campaign runs grows or shrinks the pool on the
fly; an explicit `AF_MAX_PARALLEL` SHALL remain a fixed ceiling across
reloads. In-flight attempts finish on the worker they started and are
unaffected by either change.

#### Scenario: hot reload refreshes ExecCtx for dispatch

- Given a campaign mid-run
- When the workers file changes (e.g. a worker is disabled) between rounds
- Then subsequent dispatch picks reflect the change without a restart

#### Scenario: adding a worker mid-run grows the pool

- Given a campaign launched with the default ceiling (AF_MAX_PARALLEL unset) and two enabled workers
- When a third worker is appended to workers.json while `af run` is running
- Then the next dispatch round runs with a pool capacity of three, so three disjoint-scope tasks may run concurrently

#### Scenario: disabling a worker mid-run shrinks the pool

- Given a campaign launched with the default ceiling and three enabled workers
- When one worker is set `enabled: false` while `af run` is running
- Then the next dispatch round runs with a pool capacity of two and no new attempt is routed to the disabled worker

#### Scenario: an explicit ceiling stays fixed

- Given a campaign launched with `AF_MAX_PARALLEL=4` and two enabled workers
- When a third worker is appended to workers.json while `af run` is running
- Then the pool capacity remains four (the operator's explicit ceiling is never recomputed)

(Shared scenarios for failure handling live in the config delta.)

### Requirement: Cooperative stop drains in-flight attempts

`af run` SHALL write a pid file (`af.pid`) into the state dir and SHALL check
a stop flag file (`stop`) each poll round; when the flag is present the loop
SHALL stop dispatching new attempts, let in-flight attempts finish through
their normal reap path, and exit non-zero with the board. The `af stop`
subcommand SHALL signal the pid recorded in the pid file (best effort) and
create the flag file. A hard kill of the orchestrator SHALL remain safe via
the existing startup heal.

#### Scenario: stop flag halts new dispatches and drains

- Given a running campaign with one in-flight attempt
- When the stop flag file appears in the state dir
- Then no new task is dispatched
- And the loop exits only after the in-flight attempt reaps, printing the
  board with a stopped marker and exiting non-zero

#### Scenario: af stop signals the pid file owner

- Given `af.pid` in the state dir naming a live af process
- When the operator runs `af stop`
- Then the orchestrator process is terminated and the flag file is created

#### Scenario: crash log captures an unexpected panic

- Given a running campaign
- When the run loop panics
- Then the panic message and location are appended to `<state dir>/crash.log`
- And the default panic output is preserved

### Requirement: Gate-failure defect detection

When a task's most recent AGENT-driven attempt (one that ran the agent, not a gate-only rerun) dies at the acceptance gate, `af run` SHALL compare its gate-error fingerprint to the fingerprint of the previous AGENT-driven gate-failed attempt of the same task. The fingerprint is the pair (exit code, whitespace-normalized tail) of the gate output as recorded on the receipt. When the two fingerprints are IDENTICAL and the task has at least two attempts recorded, `af` SHALL mark the task `failed` immediately — even while attempts remain under the retry budget — with the reason `repeated identical gate failure (<tail>) — task design defect`, persist it, print the early stop, and SHALL NOT dispatch further attempts for that task. A differing fingerprint (a flake, or an agent whose new work moved the gate's failure) SHALL fall through to the normal retry decision unchanged. Gate-only reruns (re-runs of the same committed work) SHALL NOT be used as comparison targets, so an identical result from re-running a single branch never triggers the stop by itself. The persisted receipts are the only evidence used, so the detection survives crash and resume.

#### Scenario: identical gate failure stops before the retry budget

GIVEN a task whose attempt 1 (a full agent run) dies at the gate with `PARSE-FAIL nix/…`, whose attempt 3 (another full agent run, after the cheap gate rerun of attempt 2) dies with the same `PARSE-FAIL` tail and exit code
WHEN attempt 3 is reaped
THEN the task is marked `failed` with reason `repeated identical gate failure (…) — task design defect`, even though `max_attempts` has not been reached, and no attempt 4 is dispatched.

#### Scenario: a different fingerprint retries normally

GIVEN a task whose attempt 1 dies at the gate with reason A and attempt 2 (fresh agent) dies with reason B (the agent changed the files the gate checks)
WHEN attempt 2 is reaped
THEN the task stays in retry flow and attempt 3 may still be dispatched.

#### Scenario: a gate-only rerun never triggers alone

GIVEN a task whose attempt 1 failed at the gate and attempt 2 was only a cheap gate rerun of the same committed work (no agent), failing identically by construction
WHEN attempt 2 is reaped
THEN no early stop fires (the comparison target must be a full agent-driven attempt with new work).

#### Scenario: detection survives resume

GIVEN a task whose two agent-driven attempts both died at the gate with identical fingerprints, split across two `af run` invocations
WHEN the second run reaps the later attempt
THEN the receipts persisted by the first run still trigger the early stop.

### Requirement: Retry prompts reuse the gate-failure receipt

The "Previous attempts" block in a retry prompt SHALL quote the receipt's `error` verbatim. Because gate-failure receipts now carry the Failing tail of the gate output (see the state delta), a task retried after a gate failure SHALL present that tail — the last lines the gate printed — to the next agent, so the agent is told why the gate rejected its work rather than shown a banner or an empty line. This is the ADR-13 mechanism unchanged; it only consumes the improved receipt content.

#### Scenario: retry prompt shows the gate's failing line

GIVEN a task whose only failed attempt died at the gate with a trailing `YAML-FAIL k8s/openedusuite/deploy-drawio.yaml` line
WHEN attempt 2 renders its "Previous attempts" block
THEN the block contains that trailing line, exactly as recorded on the receipt.

#### Scenario: first attempt and non-gate failures are unchanged

GIVEN a task whose attempt 1 failed on a provider timeout (no gate)
WHEN attempt 2 renders
THEN the block quotes the timeout's first line as before, and a first attempt renders no block at all.
