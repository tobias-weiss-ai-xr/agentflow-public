# examples/simple — a minimal agentflow campaign

The smallest complete `af` campaign: **two tasks** chained by a dependency and
**one worker** using af's built-in agent harness (`"cli": "builtin"`), so no
external agent CLI needs to be installed.

```
hello-module  ──deps──▶  hello-docs
```

- `hello-module` — add `src/hello.py` to the target repo (defines `hello()`).
- `hello-docs` — add `docs/hello.md` documenting the module; only becomes
  ready once `hello-module` reaches `done`.

## Files

| File | What it is |
|------|------------|
| `tasks.json` | `{"tasks": [...]}` — each task: `id`, `title`, `deps`, `scope` (file globs the task may touch), `accept` (shell gate, exit 0 = pass), `acceptance_prose`, `manual` |
| `workers.json` | `{"defaults": {...}, "workers": [...]}` — defaults: `accept_timeout_s`, `max_attempts`, `retry_delay_s`, `agent_timeout_s`; each worker: `name`, `provider`, `model`, `api_base`, `api_key_env`, `enabled`, `cli` |

## Before you run — replace the placeholders

- **`api_base`** (`http://localhost:8000/v1`) — must be a real
  OpenAI-compatible `/chat/completions` endpoint: OpenAI, OpenRouter, z.ai,
  a litellm proxy, a local vLLM server, …
- **`model`** (`gpt-4o-mini`) — any model your endpoint serves.
- **`api_key_env`** (`EXAMPLE_API_KEY`) — the name of the env var af reads
  the API key from; export that variable with your key (a local server that
  needs no key still needs the variable exported, even empty).

## How to run

```sh
# 1. Point af at the repo the tasks should modify, and give it a state dir
export AF_REPO_DIR=/path/to/repo/being/modified
export AF_STATE_DIR=state

# 2. Your key, named by api_key_env in workers.json
export EXAMPLE_API_KEY=sk-...

# 3. Pre-flight: schema, dependency cycles, duplicate ids, no enabled workers.
#    Dispatches nothing.
af validate --tasks examples/simple/tasks.json --workers examples/simple/workers.json

# 4. Preview the plan and budget. Creates nothing.
af run --dry-run --tasks examples/simple/tasks.json --workers examples/simple/workers.json

# 5. The campaign: worktree → agent → gate → merge, with retries on failure.
#    hello-docs starts only after hello-module merges.
af run --tasks examples/simple/tasks.json --workers examples/simple/workers.json
```

(If you built from source, use `./target/release/af` in place of `af`.)

Tip: the same paths can be set via `AF_TASKS_JSON` / `AF_WORKERS_JSON`
instead of the flags.

## What success looks like

`af run` exits `0` when every in-scope task reaches `done` — each attempt
merged its branch and passed its `accept` gate. Inspect the receipts and
spend afterwards with `af status` and `af cost`.
