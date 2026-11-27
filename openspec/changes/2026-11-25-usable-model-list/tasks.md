# Tasks: usable-model-list

## 1. Harness: excluded-model validate warning (config)

- [x] 1.1 Add `KNOWN_PROBLEM_MODELS: &[&str]` in `src/config.rs` (denylist:
  `kimi`), with a comment naming `docs/workers.md` as the evidence ledger +
  maintainer rule and explaining why `glm-5.2-awq` is deliberately NOT on it
  (concurrency problem → serialized default, not exclusion).
- [x] 1.2 In `validate`'s per-worker loop (next to the `api_key_env` warning):
  when `w.enabled` and the model matches (case-insensitive substring), push a
  warning naming the worker, the model, and pointing at the proven
  replacement set + `docs/workers.md`. Disabled workers never warn.
- [x] 1.3 Unit tests in `src/config.rs`: enabled `tud/kimi-k2.6` and bare
  `kimi` warn; mixed-case `tud/Kimi-K2.6` warns; disabled kimi worker does
  not warn; `tud/glm-5.2-awq` with `max_in_flight: 1` does NOT warn; proven
  `zai/glm-5.3-flash` never warns. Traceability markers to the spec
  scenarios.

## 2. Usable model list (docs)

- [x] 2.1 New `docs/workers.md`: canonical usable-model list — PROVEN /
  CONDITIONAL / EXCLUDED with one evidence line per row, an
  instance-defaults table (mistral 2, awq 1 serialized), and the maintainer
  rule ("a model earns a denylist slot only after an operator directive or
  two independent campaign failures; edit the denylist in `src/config.rs` in
  the same shape as this file").
- [x] 2.2 Link `docs/workers.md` from the fleet overview doc and from
  `README.md` (worker intro).

## 3. Instance defaults (scripts)

- [x] 3.1 the fleet generation script `write_workers()`: herd =
  `tud-mistral` (`max_in_flight: 2`), `tud-glm52-awq` (`max_in_flight: 1`,
  serialized on purpose), `zai-glm53-flash` (1), `local-dsv4` (1). Heredoc
  comment carries the instance rationale + exclusions (Kimi). Provider stays
  `litellm` throughout.

## 4. Spec + ship

- [x] 4.1 `openspec validate` green for the change; config spec delta covers
  the excluded-model warning AND documents `max_in_flight` (0 → 1 in-flight,
  N → N instances).
- [x] 4.2 README pointer done (worker intro -> `docs/workers.md`).
- [ ] 4.3 Gates:
  `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`,
  `cargo test`, `./scripts/coverage-gate.sh` (ratchet must not fall).
- [ ] 4.4 Commit (change-scoped).
