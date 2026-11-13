# Quickstart: zero to first merged task in five minutes

This walkthrough takes a newcomer from an empty checkout to a task planned,
validated, executed by an agent, gated, and merged — using one small task and
one worker. Full field references live in the README's
[Task schema](../README.md#task-schema-configtasksjson) and
[Worker schema](../README.md#worker-schema-configworkersjson) sections; the
[CLI](../README.md#cli) section lists every command used here.

Everything below uses the af binary directly. No external agent CLI is
installed on your machine — the worker uses af's built-in in-process harness.

## What you need

- Rust (stable) with `cargo` on your `PATH` — see `rust-toolchain.toml` for the
  pinned toolchain.
- `git` — every task runs on its own branch in its own worktree.
- Any OpenAI-compatible `/chat/completions` endpoint and its API key: OpenAI,
  OpenRouter, z.ai, a litellm proxy, or a local vLLM all work.
- The repository you want af to modify, cloned locally. For a first run, use
  af's own checkout or any small scratch repo — the task below is trivial.

## 1. Build af

```sh
cargo build --release            # produces target/release/af
./target/release/af --version
```

The release build is the artifact you will run everything through below;
`af --version` prints a git-derived version such as `0.1.0+<commitcount>.<shorthash>`.

## 2. Declare one task

Tasks are JSON. Start with one small task whose acceptance gate is a shell
command that must exit `0` before the branch can merge:

```json
// config/tasks.json
{ "tasks": [ {
    "id": "hello",
    "title": "Add a hello module",
    "scope": ["src/**"],
    "accept": "test -f src/hello.rs",
    "acceptance_prose": "src/hello.rs exists and declares pub fn hello()."
} ] }
```

What each field means:

- `id` — unique task name; it becomes the branch and the receipt key.
- `title` — the goal handed to the agent in its prompt.
- `scope` — globs of files the task may touch. An attempt that edits anything
  outside the scope is rejected before the gate runs.
- `accept` — the shell command that decides success. Keep it exact and cheap.
- `acceptance_prose` — one sentence of intent, included in the agent's prompt
  so it knows what "done" means beyond the raw command.

## 3. Declare one worker

Workers describe how an agent is reached. `"cli": "builtin"` selects af's
native in-process harness — no external agent CLI is installed, af talks to
the model itself:

```json
// config/workers.json
{ "defaults": { "accept_timeout_s": 600, "max_attempts": 2 },
  "workers": [ {
    "name": "glm",
    "provider": "zai",
    "model": "glm-5.2",
    "api_base": "https://api.z.ai/api/paas/v4",
    "api_key_env": "ZAI_API_KEY",
    "enabled": true,
    "cli": "builtin",
    "max_turns": 48
} ] }
```

- `api_base` — any OpenAI-compatible `/chat/completions` URL (z.ai, OpenRouter,
  a litellm proxy, a local vLLM…). For OpenAI proper you would use
  `https://api.openai.com/v1` with `"provider": "openai"`.
- `api_key_env` — the name of the environment variable af reads the key from.
  af never stores the key in the config file.
- `enabled: true` — a disabled worker is skipped; a config with no enabled
  workers fails validation.
- `defaults` — retry (`max_attempts`) and gate timeout shared by all workers.

## 4. Point af at your repository

af needs two directories, and both must be **absolute paths**:

```sh
export AF_REPO_DIR=/abs/path/to/the/repo/being/modified   # the repo tasks modify
export AF_STATE_DIR=/abs/path/to/af-state                 # receipts + worktrees live here
export ZAI_API_KEY=...                                    # whatever api_key_env names
```

- `AF_REPO_DIR` — the repository tasks modify; it is the base for branches,
  worktrees and merges. Use the clone from "What you need".
- `AF_STATE_DIR` — where receipts, the pid file and the task worktrees live.
  It can be anywhere; it must not be inside the repo you are modifying.

Absolute paths keep the run, the receipts and the worktrees pointing at the
same place no matter which directory you invoke `af` from. The defaults are
relative to the current directory (`AF_TASKS_JSON=config/tasks.json`,
`AF_WORKERS_JSON=config/workers.json`), which is why the commands below are
run from af's checkout — you can also export `AF_TASKS_JSON`/`AF_WORKERS_JSON`
as absolute paths to pin them.

## 5. Pre-flight: validate, then preview

Always do this before spending a token. Two commands, neither dispatches an
agent:

```sh
# 1. Config check: schema, dependency cycles, duplicate ids,
#    no enabled workers, touch/scope coverage — dispatches nothing.
./target/release/af validate

# 2. Dry run: prints the plan and the budget, creates nothing.
./target/release/af run --dry-run
```

`af validate` catches the boring failures — a typo'd field, a dependency
cycle, two tasks with the same id, zero enabled workers — while `af run
--dry-run` shows the execution plan (which tasks run when, on which worker)
and the current spend line. If the plan looks wrong here, it is free to fix.

## 6. Run the campaign

```sh
./target/release/af run
```

The orchestrator loop, per task: create a worktree on a fresh branch → hand
the task to the worker's agent → run the `accept` gate in that worktree →
verify the scope was respected → merge into the base. Failed attempts retry
(`max_attempts`) on a fresh worktree, with the previous failures fed back into
the retry prompt. `af run` exits `0` when every in-scope task reached `done`,
`2` on a configuration error or deadlock, `3` when a budget ran out early.

## 7. Watch the board and the bill

While `af run` is going — or after it finishes:

```sh
./target/release/af status      # per-task board: done / running / failed
./target/release/af cost        # the spend ledger, per-worker trust, waste
```

`af status` shows every task and its attempt state; `af status --json` is the
machine-readable form, `af attach <ID>` tails a running attempt's log.
`af cost` accumulates wall-clock, turns and token spend per worker, plus
waste — tokens paid for attempts that failed their gate.

When `hello` shows `done`, the commit is in `AF_REPO_DIR`'s branch and merged
base — check it with `git log` and delete `AF_STATE_DIR` when you want a
clean slate.

## Troubleshooting

- **`af validate` rejects the config** — read the error; it names the task and
  the rule (duplicate id, cycle, no enabled worker, `touch` file not covered
  by any `scope` glob).
- **`af run --dry-run` prints a budget-exhausted line immediately** — a
  previous campaign's receipts in `AF_STATE_DIR` already count; point
  `AF_STATE_DIR` at a fresh directory for a clean start.
- **Agent cannot reach the model** — `api_key_env` names a variable that must
  be exported in the shell running `af run`; `provider` must match the
  endpoint at `api_base`.
- **Gate keeps failing** — run the `accept` command by hand in a scratch
  checkout; a gate that cannot pass without the agent's work still needs to
  be *satisfiable*, and `acceptance_prose` is how the agent learns what it
  must create.
- **Task failed but the work looked fine** — the attempt's branch was archived
  as `<branch>-rejected-*`; `af recover --task <ID>` (or the next `af run`)
  can land it without paying for it twice.
