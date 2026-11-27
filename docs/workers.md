# agentflow — usable model list

The fleet policy earned across ~10 agentflow campaign families
(long-running campaign parity L2/L2D–L2J, port-1000 loop, OO 2.2/2.3 slices,
improvement campaign). If you hand-write a `workers.json`, start from
PROVEN. **`af validate` warns** (never blocks) when an enabled worker's
model id matches an EXCLUDED entry — the denylist lives in
`src/config.rs::KNOWN_PROBLEM_MODELS` and is the enforcement of this file.

## Maintenance rule

- A model **earns an EXCLUDED slot only after two independent campaigns**
  confirm the same failure mode, or an explicit operator directive. One
  off-run is a fluke; two is a pattern.
- Updates ship as **one commit touching both** this file and
  `KNOWN_PROBLEM_MODELS` in `src/config.rs` (they are a pair — the doc is
  the evidence ledger, the constant is the check).
- Prices/turn caps change; the *usable* verdict is about failure modes, not
  cost.

## Instance defaults (2026-11-25, fleet `write_workers()`)

Two TUD workers carry **explicit `max_in_flight`** in the default fleet —
the concurrency each backend earned:

| Model | Default instances | Why |
|---|---|---|
| `tud/mistral-medium-3.5-128b` | **2** (`max_in_flight: 2`) | Strongest verified builtin tool-caller (double-slot directive 2026-11-24); proven to 5× parallel (2026-11-11); stayed responsive while the awq backend saturated on the same gateway (2026-11-25). 2 = its earned headroom. |
| `tud/glm-5.2-awq` | **1** (`max_in_flight: 1`, serialized on purpose) | Its chronic stalls are **backend self-saturation under parallel load**, not quality — see CONDITIONAL below. One instance never stacks on the shared vLLM backend. **Do not raise this.** |

`zai/glm-5.3-flash` and `local/deepseek-v4-flash/ai1` default to 1 lane
each. Kimi has no instances — it is excluded.

## PROVEN — keep, promote

| Model | Route(s) | Record |
|---|---|---|
| `zai/glm-5.3-flash` | `zai/glm-5.3-flash` via litellm | Best campaign worker ever measured: L2 15→65, L2D 68→129, L2G 168→241, L2H 241→293, L2I, L2J — consistently 10/10, 6/6, 6/6 on **attempt 1**, 0 FAIL, zero wasted attempts (2026-11-04→06). |
| `local/deepseek-v4-flash/ai1` | `local/deepseek-v4-flash/ai1` via litellm :8888 / :4000 | Reliable across taskfleet, fleet, advisory (7 s clean JSON on census advisory, `--chunk 8`), improvement campaign worker. Cheap (0.3 $/Mtok). |
| `tud/mistral-medium-3.5-128b` | `tud/mistral-medium-3.5-128b` via litellm | Strongest verified **builtin-harness** tool-caller; double-slot in the fleet herd (2026-11-24). Stayed responsive when the same gateway's `glm-5.2-awq` backend saturated (2026-11-25). Caveat → CONDITIONAL. |

## CONDITIONAL — usable with a caveat

| Model | Caveat |
|---|---|
| `zai/glm-5.2` (API-hosted, **not** the TUD awq build) | Fine for coding (README canonical worker example). **Reasoning models can't run advisory/JSON-gate roles**: burn the whole `max_tokens` on `reasoning_content` and return `content=None` (`'NoneType' object has no attribute 'strip'`) — observed for glm-5.2, qwen3.5, mistral-medium (2026-10-31). |
| `tud/mistral-medium-3.5-128b` via **opencode adapter** | 2026-10-10 vLLM `Duplicate tool call id chatcmplt` on opencode parallel reads; standalone curl + builtin harness fine. Use `cli: "builtin"` (default: 2 lanes). |
| `tud/glm-5.2-awq` | **Usable, serialized: `max_in_flight: 1` (fleet default).** Every chronic failure was backend self-saturation under parallel load — 74 s per 12 completion tokens + repeated 45–60 s probe timeouts while the same gateway's mistral route stayed responsive (2026-11-25); opencode→litellm `Timeout on reading data from socket` and "no change after 57s" on a reused state dir (2026-10-10). One instance executes fine; parallel slots recreate the stall. **Do not raise the concurrency.** |
| `litellm/glm-4.7` | Historical AF4-6: 3/3 attempt-1 (2026-10-25), then rate-limited (Weekly/Monthly Limit Exhausted). Fine when quota available. |
| `saia/qwen3.5-397b-a17b` | Coding OK; advisory excluded (reasoning, see above). Some campaigns reported "needs strong worker or manual" for hard merge-conflict rows. |

## EXCLUDED — chronic failure record (`KNOWN_PROBLEM_MODELS`)

| Model id substring | Evidence (two independent confirmations minimum, or operator directive) |
|---|---|
| `kimi` (`tud/kimi-k2.6`, `kimi-*`) | 2026-10-10 OO 2.2/2.3: Kimi drift ×1 + crash mid-task. 2026-11-24: **operator-excluded** from the fleet ("no Kimi"). |

## See also

- `src/config.rs::KNOWN_PROBLEM_MODELS` — the enforcement side of this file.
- `README.md` worker configuration section — worker schema (`api_base`, `model`, `max_in_flight`).
