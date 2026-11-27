# Proposal: usable-model-list (documented defaults + excluded-model warning)

## Why

Across every agentflow campaign family (the parity L2/L2D–L2J campaigns, the
port-1000 loop, OO 2.2/2.3 slices, the improvement campaign), the fleet's
reliability picture is now clear enough to write down as defaults instead of
tribal knowledge:

- **`tud/mistral-medium-3.5-128b` is the fleet's strongest worker** — proven
  to 5× parallel (2026-11-11), stayed responsive while the awq backend
  saturated on the same gateway (2026-11-25), double-slot directive
  (2026-11-24). It earns `max_in_flight: 2` by default.
- **`tud/glm-5.2-awq`'s chronic stalls are a concurrency problem, not model
  quality** — backend self-saturation: 74 s per 12 completion tokens and
  repeated 45–60 s timeouts while same-gateway mistral stayed responsive
  (2026-11-25); opencode→litellm socket timeouts (2026-10-10). The fix is a
  **serialized default (`max_in_flight: 1`)**, not exclusion.
- **Kimi stays excluded** — operator directive (2026-11-24, "no Kimi") after
  drift + mid-task crashes (2026-10-10).
- **`zai/glm-5.3-flash` is the best attempt-1 worker ever measured** (10/10,
  6/6, 6/6 clean first-attempt gates) and **`local/deepseek-v4-flash/ai1`**
  is the reliable cheap local; both get 1 default lane.

Yet today these verdicts live only in prose. There is no committed **usable
model list**, the `write_workers()` heredoc is the only operational
expression of the fleet, `max_in_flight` is implemented in the dispatcher but
absent from the spec, and `af validate` (which already warns on unset
`api_key_env` — the warning that let the port-1000 forensics reclassify
160/167 "agent" failures as config) says nothing about an **excluded** model
id, so a stale familiar workers.json burns its `max_attempts` budget before
any operator sees a signal.

## What Changes

- **Instance defaults set in the fleet** — the fleet generation script
  `write_workers()`: `tud-mistral` `max_in_flight: 2`, `tud-glm52-awq`
  `max_in_flight: 1` (serialized on purpose), `zai-glm53-flash` 1,
  `local-dsv4` 1. Each with the campaign evidence in the heredoc comment.
- **`docs/workers.md`** — the committed usable-model list: PROVEN
  (glm-5.3-flash, deepseek-v4-flash/ai1, mistral-medium) / CONDITIONAL
  (awq — serialized; glm-5.2, qwen3.5, mistral-medium excluded from
  advisory-JSON roles; opencode-adapter caveat) / EXCLUDED (Kimi — operator
  directive), plus the instance-defaults table and the maintainer rule.
- **`af validate` warns** (never errors; ADR-1 pass-through preserved) when
  an **enabled** worker's model id matches `KNOWN_PROBLEM_MODELS` (`kimi`),
  naming the worker, the model, and the replacement set. Disabled workers
  never warn. The list is deliberately tiny — a model earns a slot only from
  an operator directive or a two-campaign failure record.
- **Spec documents `max_in_flight`** (current dispatcher behavior: `0` → one
  in-flight attempt per worker, `N` → up to N concurrent — one entry, N
  instances) so the fleet defaults are a spec-backed contract, not a script
  quirk.
- **Docs pointers**: fleet overview + repo README link to
  `docs/workers.md`.

## Non-Goals

- No hard block at load time (warning only), no price/route changes, no
  changes to the in-flight cascade-routing / token-budget / taskfile changes,
  no changes to worker CLI semantics, no rewrite of historical dogfood
  configs that ran on `glm-5.2-awq` (they are truthful records of their
  rounds).
