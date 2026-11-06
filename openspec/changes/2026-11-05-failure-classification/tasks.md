# Tasks: failure-classification

## 1. Receipt field + verdict semantics

- [x] 1.1 `Receipt.failure_class: Option<String>` (serde default None) — `"infra"` | `"agent"` | None
- [x] 1.2 `counts_as_verdict` returns false when `failure_class == Some("infra")`
- [x] 1.3 state tests: infra failed is not a verdict; agent failed is; legacy (None) still is
- [x] 1.4 All Receipt initializers updated (src + tests)

## 2. Classifier: single source of truth

- [x] 2.1 `pub fn classify_failure(&str) -> &'static str` in execute.rs (infra signals: harness/timeout/stall/missing/kill/cannot/provider/connection)
- [x] 2.2 `execute_task` stores `failure_class` on failed receipts (classified from full reason before truncation)
- [x] 2.3 Unit tests: infra signals classify infra; agent signals classify agent

## 3. Trust wiring

- [x] 3.1 `reap` skips the live `router.record` for infra-classified failures
- [x] 3.2 Router tests: infra failure (filtered by counts_as_verdict) leaves trust None; agent failure still lowers trust + pick deprioritizes

## 4. Visibility

- [x] 4.1 `status_json` surfaces each task's most recent `failure_class` (null when none)
- [x] 4.2 Test: status --json carries failure_class for a failed task

## 5. Spec + README + ship

- [x] 5.1 State spec requirement "Failure class on receipts" + scenarios; traceability markers on tests
- [x] 5.2 README: answer feedback §3 (infra failures no longer poison trust)
- [x] 5.3 `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, full `cargo test` green
- [x] 5.4 `openspec validate` green; commit
