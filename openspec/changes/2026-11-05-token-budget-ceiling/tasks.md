# Tasks: token-budget-ceiling

## 1. Config: `Settings::max_tokens` + `Task::max_tokens`

- [x] 1.1 Add `max_tokens: u64` to `Settings` (env `AF_MAX_TOKENS`, default 0 = unlimited), read in `from_env` alongside `max_wall_clock_s`
- [x] 1.2 Add `max_tokens: Option<u64>` to `Task` (serde default `None` = no per-task cap); reject negative in `load` validation
- [x] 1.3 Unit test: `AF_MAX_TOKENS=0` → unlimited (default); `AF_MAX_TOKENS=500000` → 500000; negative → rejected
- [x] 1.4 Unit test: task with `max_tokens: 0` or absent → no per-task cap; `max_tokens: 1000` → cap is 1000

## 2. Run loop: campaign-level token ceiling

- [x] 2.1 Add `consumed_tokens(receipts, in_scope)` and `token_budget_exhausted(st, consumed)` in `run.rs`, parallel to the wall-clock functions
- [x] 2.2 Extend the `over_budget` check: `over_budget = wall_clock_budget_exhausted(...) || token_budget_exhausted(...)`
- [x] 2.3 Cache `consumed_tokens` alongside `measured_cache` (updated in the same reap/reload block)
- [x] 2.4 Diagnostic: name which ceiling was hit (token vs wall-clock) in the `eprintln!`
- [x] 2.5 `--dry-run` budget line: print token ceiling alongside wall-clock when non-zero
- [x] 2.6 Test: campaign with `AF_MAX_TOKENS=1000` and receipts totaling 1000+ tokens → exit 3, no new dispatch
- [x] 2.7 Test: campaign with `AF_MAX_TOKENS=0` → unlimited (no stop)

## 3. Run loop: per-task token ceiling

- [x] 3.1 Before dispatch (after reuse check, before worker selection), check `task.max_tokens` against the task's receipt token sum; if spent >= cap, set `Failed` with reason, save, `continue`
- [x] 3.2 Test: task with `max_tokens: 500` and receipts totaling 500+ tokens → task set to `Failed`, not dispatched
- [x] 3.3 Test: task with `max_tokens: 500` and receipts totaling 499 tokens → dispatched normally
- [x] 3.4 Test: `max_tokens: 0` or absent → no per-task cap

## 4. Spec + README + ship

- [x] 4.1 Add requirement + scenarios to `openspec/specs/scheduling/spec.md`
- [x] 4.2 `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, full `cargo test` green
- [x] 4.3 `openspec validate --all --strict` green
- [x] 4.4 README: document `AF_MAX_TOKENS` and `max_tokens` alongside `AF_MAX_WALL_CLOCK_S` and `max_turns`
- [x] 4.5 Commit + push
