# Design: failure-classification

## D1 — One classifier, both the receipt and the live router

`classify_failure(error: &str) -> &'static str` lives in `execute.rs` and
matches the controlled prefixes agentflow's failure reasons actually
produce, lowercased:

```
INFRA signals: "harness" | "timeout" | "stall" | "missing" | "kill"
               | "cannot" | "provider" | "connection"
              → "infra"
else          → "agent"
```

- `harness` covers every native-harness stop: `harness: turn cap N reached`,
  `harness: <provider error>` (429/5xx/connect), `harness failed (Timeout…`,
  `harness failed (Stalled…`, `harness failed (ProviderError…)`.
- `timeout` / `stall` / `missing` / `kill` cover the CLI `CmdKind` reasons
  (`agent exited Timeout`, `agent stalled: …`, `agent exited Missing`,
  `agent exited Killed`).
- `cannot` covers git/io errors (`cannot write prompt`, `cannot commit the
  agent's uncommitted work`, `cannot compute attempt diff`).
- Everything else is `agent`: `agent exited NonZero`, `gate failed`,
  `scope violation`, `zero commits`, merge-verification failure, `no change
  produced`.

`execute_task` classifies the FULL reason before truncation and stores it on
the receipt. `reap` classifies the same string from the `Outcome::Failed`
payload when deciding the live router update. Both call the same function →
replay and runtime agree by construction.

## D2 — `counts_as_verdict` excludes infra only

```rust
pub fn counts_as_verdict(&self) -> bool {
    (self.outcome == "merged" || self.outcome == "failed" || self.outcome == "verified")
        && self.failure_class.as_deref() != Some("infra")
}
```

This single change routes through every trust consumer:
- run-loop startup replay filters through `counts_as_verdict` before
  `Router::from_receipts` (no code change there — the filter now drops infra).
- `reap`'s live `router.record` is guarded explicitly (it sees `Outcome`,
  not receipts): infra failures skip `record` entirely.
- `af cost` `WINS/TOTAL` and `MEAN_S` filter through `counts_as_verdict`.

An infra failure still: counts toward the task's status `attempts` and
`max_attempts` comparison (reap increments `s.attempts` unconditionally),
and appears in cost totals and `WASTED BY REASON` (they filter by outcome,
not verdict). Only trust ignores it.

## D3 — status --json surfaces the latest failure class

`status_json` scans receipts for each task and finds the most recent
`failed` receipt; its `failure_class` becomes the task's `failure_class`
field (`null` when none). No schema breakage: a new optional JSON key; the
existing `error`/`verdict`/`contracts` fields are unchanged.

## D4 — Backward compatibility

`failure_class` is `#[serde(default)]` → legacy receipts deserialize with
`None` → `counts_as_verdict` returns the historical answer for them (a
`failed` legacy receipt is a verdict, as today). All bookkeeping receipts
(recovered/verified/interrupted) and their tests set `failure_class: None`
explicitly.
