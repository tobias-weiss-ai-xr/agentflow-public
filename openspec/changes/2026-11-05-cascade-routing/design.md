# Design: cascade-routing

## D1 — Field and validation

`Task::cascade: Option<Vec<String>>`, `#[serde(default)]` (absent → UCB1 as
before). Validation (in `config::validate`, hard errors):

- empty array → `task '<id>': cascade is an empty array — omit it instead`
- entry names no worker in `workers.json` → error naming task + worker
- duplicate entry within one cascade → error naming task + repeated worker
- first entry names a disabled worker → non-fatal warning (every attempt
  falls back to UCB1)

## D2 — dispatch integration (single decision point)

The dispatch loop reads the task's NEXT attempt number from persisted status
(`status.attempts + 1`) BEFORE worker selection — the same number the
subsequent save persists — then tries, in order:

1. `cascade_worker(&cfg, &t, next_attempt, &worker_busy)` — returns the
   cascade worker when: a cascade exists, `next_attempt - 1 < len`, the
   named worker is enabled, and it is not busy.
2. `pick_worker(&cfg, &worker_busy, &router, filter)` — the existing UCB1 —
   used when the cascade applies nothing.

```rust
fn cascade_worker<'a>(cfg, task, attempt, busy) -> Option<&'a Worker> {
    let cascade = task.cascade.as_ref()?;
    if attempt == 0 { return None; }
    let name = cascade.get((attempt - 1) as usize)?;
    cfg.workers.iter().find(|w| w.name == *name
        && w.enabled
        && !busy.get(&w.name).copied().unwrap_or(false))
}
```

No other part of the pipeline changes: the receipt records whichever worker
ran, the gate adjudicates identically, and `attempts`/`max_attempts` bounds
escalation exactly as they bound retries today. An explicit `--worker`
filter overrides the cascade for every attempt (the operator's pin is a
stronger constraint than the task's escalation intent).
