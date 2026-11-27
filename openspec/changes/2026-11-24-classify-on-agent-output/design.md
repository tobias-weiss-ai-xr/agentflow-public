# Design: classify-on-agent-output

## Goal

Make `failure_class` reflect WHO failed (the infrastructure or the agent),
based on what the agent actually said — and never let a dirty merge target
destroy a successful attempt. Fix it once, where every caller routes through.

## Root causes (from receipts, not guesses)

| # | Symptom | Root cause | Fix site |
|---|---|---|---|
| 1 | 160/167 `agent`-class failures die in 5–7 s | CLI path builds the reason from the exit wrapper only; `classify_failure` keyword-matches that wrapper → `agent` | `execute_attempt` reason builder |
| 2 | `af status`/retry prompt say "NonZero (code 1)" | same as #1 — no diagnosis in the string | same |
| 3 | 6 successful attempts lost at merge | `worktree::merge` never checks `is_dirty(repo)` | `worktree::merge` + dispatch pre-flight |

## Mechanism

### 1. Cause line diagnosis on the CLI failure path

`CmdOut` already carries `stdout`/`stderr`. In the `!committed_ahead` branch
of `execute_attempt` (the only place a CLI non-zero becomes a failure
reason), `last_output_line(&agent_out)` derives the diagnosis:

- **first** non-empty line of the combined output that `classify_failure`
  maps to `infra`, falling back to the **last** non-empty line when none
  does; ANSI escapes stripped, capped at 160 chars → `; last output: <line>`
- no output at all → `agent produced no output (exit N) after Ts`

Why *first cause line* rather than the literal last line: the real
port-1000 incident (164 identical deaths) printed

```
Warning: Model "tud/x" not found for provider "openai". Using custom model id.
No API key found for openai.

Use /login to log into a provider via OAuth or API key. See:
  /home/…/pi-coding-agent/docs/providers.md
  /home/…/pi-coding-agent/docs/models.md
```

The literal last line is a docs path (unclassifiable); the last *signal*
line is the `Use /login` remediation hint; the **first** signal line is the
cause. Both cause lines classify `infra`, which is the contract; the test
asserts either.

The reason becomes `agent exited NonZero (code 1); last output: Request was
aborted`. Callers that match on the historical shape (`starts_with("agent
exited")`) are unaffected because the prefix is byte-identical; the suffix
is additive. Receipt cap: `execute_task` keeps its 200-char first-line rule,
so the wrapper + diagnosis fit.

`classify_failure` gains the observed transport vocabulary (`aborted`,
`empty response`, `rate limit`, `429`, `503`, `overloaded`, `socket`, `eof`,
`reset by peer`, `dns`). Order of checks unchanged; still one function; the
receipt and the live router still agree because both call it.

Why not classify inside `subprocess.rs`? Because whether a non-zero exit is
"the agent's fault" depends on whether it committed work (judged in
`execute_attempt`), and because the native-harness path already emits
`harness: <e>` — keeping classification at the reason-string level keeps
both worker kinds on one path (ADR-13 "single source of truth").

### 2. Merge pre-flight

```rust
pub fn merge(repo, branch, locks, msg) -> Result<(), String> {
    let _guard = locks.0.lock()...;
    if let Some(f) = first_dirty_path(repo)? {          // NEW
        return Err(format!("merge target is dirty ({f}); commit or stash before af merges"));
    }
    let out = git(repo, &["merge", "--no-ff", branch, "-m", msg]);
    ...
}
```

No `git merge --abort` on this path (nothing ran). The operator's working
tree is never modified. Belt: catches a tree dirtied mid-attempt.

### 3. Dispatch pre-flight (suspenders)

In `run_loop`, before handing a task to a worker: `is_dirty(repo)`. If
dirty, skip dispatch for that task THIS round (no receipt, no attempt
consumed), log the reason once per dirty-file set, and continue with other
tasks. The loop already re-reads state each cycle, so the moment the
operator commits/stashes, dispatch resumes automatically. `af status` shows
`waiting: merge target dirty (<file>)`.

Why not fail the task? A dirty checkout is the operator's state, not a
verdict on the task or worker. Failing would burn `max_attempts` on a
condition the retry cannot fix — the exact anti-pattern #1 already causes.

## Alternatives rejected

- **Classify on exit code alone** (status quo): exit codes carry no cause.
- **Parse agent-specific formats per CLI**: brittle, unbounded; the last
  line is CLI-agnostic and what humans read in the log anyway.
- **Retry infra failures without consuming `max_attempts`**: right in
  principle, but needs a separate infra-retry budget to avoid infinite
  loops on a dead gateway. Deferred — with correct classification the
  operator/router at least stops blaming the model.
- **Auto-stash the main checkout before merge**: touching the operator's
  working tree is never af's call (ADR-10 spirit: af does not own the
  operator's checkout beyond the merge it was asked to do).

## Deferred (research-grounded, not needed for the root cause)

- **Cross-task failure recall** (Trial by Trace, T2A): ADR-13 keeps
  episodic memory per-task; a shared "infra incident timeline" per gateway
  would let the loop skip a dead gateway for N minutes. Needs a new state
  file — a separate change.
- **Gateway health probe before dispatch** (Agent Harness Awareness): a
  cheap `GET /models` per worker before dispatch. Same state file as above.
- **Per-contract regression attribution** (`accept_report.json` already
  parsed): surface which contract regressed vs. was never green — the wo
  gate distinguishes `rev N -> 0` (regression) from `rev 0 -> 0` (absent).

## Test plan (ADR-5 pyramid)

Unit (no I/O):
- `classify_failure("agent exited NonZero (code 1); last output: Request was aborted") == "infra"`
- `classify_failure("agent exited NonZero (code 1); last output: Error: socket hang up") == "infra"`
- `classify_failure("agent exited NonZero (code 1); last output: I could not find the file") == "agent"`
- `classify_failure("agent produced no output (exit 1) after 5s") == "infra"`
- `last_line_diag(&out)` helper: picks last non-empty line, strips ANSI, caps 160.

Integration (scratch git repo + `example_agent`):
- example_agent exits 1 printing `Request was aborted` without committing →
  receipt `failure_class == "infra"`, `error` contains `last output`.
- example_agent exits 1 silently → receipt contains `agent produced no output`.
- dirty main + passing task → attempt NOT lost: merge returns the dirty
  error; the branch is preserved (archived, not deleted); `git status`
  shows the operator's edit intact.

E2E: existing suite must stay green; add one case "dirty base parks dispatch,
resumes after commit".
