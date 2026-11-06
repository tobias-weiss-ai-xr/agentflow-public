# Tasks: cascade-routing

## 1. Config: field + validation

- [x] 1.1 `Task::cascade: Option<Vec<String>>` (serde default None), doc
- [x] 1.2 validate: unknown worker → hard error; duplicate entry → hard error; empty array → hard error; disabled first rung → warning
- [x] 1.3 config tests: accepts matching workers; rejects unknown; rejects duplicate; rejects empty; disabled-first-rung warns

## 2. Dispatch integration

- [x] 2.1 `cascade_worker` helper (attempt-indexed, busy+enabled checks, past-cascade → None) and `pick_for_attempt` (cascade → UCB1 fallback; `--worker` pin overrides cascade)
- [x] 2.2 dispatch reads next_attempt from status before worker selection; uses `pick_for_attempt`
- [x] 2.3 unit tests: cheapest-first + escalation by attempt; past-cascade → None; no cascade → UCB1; busy/disabled fallback; filter override

## 3. Spec + README + ship

- [x] 3.1 scheduling spec requirement "Cascade (escalation routing)" + scenarios; config spec task fields document `cascade`
- [x] 3.2 traceability markers (standalone `// spec:` lines) on unit tests
- [x] 3.3 README: task schema row + routing section note
- [x] 3.4 `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, full `cargo test` green (455)
- [x] 3.5 `openspec validate --all` green (10/10); commit
