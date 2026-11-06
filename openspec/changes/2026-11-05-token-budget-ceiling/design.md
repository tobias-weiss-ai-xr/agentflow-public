# Design: token-budget-ceiling

## D1 — The ceiling reads the same ledger as the report

The wall-clock ceiling uses `consumed_wall_clock_s(receipts, in_scope)` —
a sum of `wall_clock_s` over in-scope receipts, filtered through
`select_receipts` (the same window `af cost` uses). The token ceiling
mirrors this exactly:

```rust
fn consumed_tokens(receipts: &[Receipt], in_scope: &[&Task]) -> u64 {
    select_receipts(receipts, &CostFilter::default())
        .into_iter()
        .filter(|r| in_scope.iter().any(|t| t.id == r.task))
        .filter_map(|r| r.tokens)
        .sum()
}

fn token_budget_exhausted(st: &Settings, consumed: u64) -> bool {
    st.max_tokens > 0 && consumed >= st.max_tokens
}
```

A receipt with `tokens: None` (interrupted, or a CLI worker without token
capture) contributes 0 — `filter_map` handles this naturally. A `failed`
attempt that burned tokens contributes them, exactly as the wall-clock
ceiling counts a failed attempt's `wall_clock_s`.

## D2 — Per-task ceiling is a pre-dispatch gate

The per-task `max_tokens` is checked in the dispatch loop, right before
`reuse_before_dispatch` and worker selection — the same place `max_turns`
escalation is resolved. A task over its per-task cap is set to `failed`
with a reason and skipped:

```rust
// Per-task token ceiling: a task whose in-scope receipts already total
// at least its max_tokens is not dispatched.
if let Some(cap) = t.max_tokens {
    if cap > 0 {
        let spent: u64 = all_receipts_cache
            .iter()
            .filter(|r| r.task == t.id)
            .filter_map(|r| r.tokens)
            .sum();
        if spent >= cap {
            let s = status.entry(t.id.clone()).or_default();
            s.state = TaskState::Failed;
            s.last_error = Some(format!(
                "token budget exhausted: spent {spent} of {cap} max_tokens"
            ));
            let _ = store.save(&status);
            eprintln!("  ⊘ {} token budget exhausted: {spent}/{cap} tokens", t.id);
            continue;
        }
    }
}
```

This composes with the campaign ceiling: the campaign ceiling stops ALL new
dispatches; the per-task ceiling stops a single task. Both are checked
before dispatch, and neither kills an in-flight attempt.

## D3 — Campaign ceiling in the same block as wall-clock

The campaign-level `over_budget` check already combines wall-clock. The
token ceiling extends it:

```rust
let measured_wall = if st.max_wall_clock_s == 0 { 0.0 } else { measured_cache };
let measured_tokens = if st.max_tokens == 0 { 0 } else { consumed_tokens(&all_receipts_cache, &in_scope) };
let over_budget = wall_clock_budget_exhausted(st, measured_wall)
    || token_budget_exhausted(st, measured_tokens);
```

The diagnostic message names which ceiling was hit:

```rust
if over_budget && ready_in_scope > 0 {
    if token_budget_exhausted(st, measured_tokens) {
        eprintln!(
            "token budget exhausted: spent {measured_tokens} of {} tokens cap; {not_started} in-scope task(s) not started",
            st.max_tokens
        );
    } else {
        eprintln!(
            "wall-clock budget exhausted: spent {measured:.1}s of {}s cap; {not_started} in-scope task(s) not started",
            st.max_wall_clock_s
        );
    }
    return 3;
}
```

## D4 — `af cost --dry-run` budget line

The existing dry-run budget line (`af cost` / `af run --dry-run`) already
prints the wall-clock ceiling. The token ceiling is printed alongside:

```
BUDGET: spent 1234s of 3600s cap (AF_MAX_WALL_CLOCK_S) — a run would stop before dispatching new attempts
BUDGET: spent 45000 of 100000 tokens cap (AF_MAX_TOKENS) — a run would stop before dispatching new attempts
```

Both lines print only when their ceiling is non-zero.

## D5 — Validation

`Settings::max_tokens` is read from `AF_MAX_TOKENS` (env, default 0).
Negative values are rejected at load (a negative token cap is nonsense).
`Task::max_tokens` is optional (`Option<u64>`); 0 or absent means no
per-task cap. A `max_tokens` on a `manual` or `readonly` task is allowed
(manual tasks have no gate, but the token ceiling is a dispatch gate, not
an acceptance gate — it stops the agent from running at all).
