# Tasks: classify-on-agent-output

## 1. Receipt diagnosis (state)

- [x] 1.1 Add `last_output_line(&CmdOut) -> Option<String>` helper in `src/execute.rs`: last non-empty line of combined output, ANSI-stripped, capped at 160 chars. Unit test.
- [x] 1.2 In `execute_attempt`'s `!committed_ahead` non-zero branch: append `; last output: <line>` when output exists, else `agent produced no output (exit N) after Ts`. Historical `agent exited …` prefix byte-identical. Integration test with `example_agent` (prints `Request was aborted`, exits 1, no commit → `failure_class == "infra"`, `error` contains `last output`).
- [x] 1.3 Extend `classify_failure` INFRA vocabulary: `aborted`, `empty response`, `rate limit`, `429`, `503`, `overloaded`, `socket`, `eof`, `reset by peer`, `dns`. Unit tests for both directions (transport → infra; `Error: file not found` → agent).
- [x] 1.4 Silent-death test: `example_agent` exits 1 with no output → `error` starts `agent produced no output`, `failure_class == "infra"`.

## 2. Merge guard (worktree)

- [x] 2.1 `worktree::first_dirty_path(repo) -> Result<Option<String>, String>` wrapping `is_dirty` + `git status --porcelain` head. Unit/integration test on a scratch repo.
- [x] 2.2 `worktree::merge`: pre-flight `first_dirty_path`; return `merge target is dirty (<path>); commit or stash before af merges` WITHOUT running `git merge` / `--abort`. Test: dirty main + passing task → operator's edit intact, branch archived, error names the file.
- [x] 2.3 Verify the existing conflict-abort path still works (`merge_conflict_aborts_cleanly` stays green).

## 3. Dispatch pre-flight (lifecycle)

- [x] 3.1 In `run_loop` before dispatch: `first_dirty_path(repo)`; if dirty, skip this task this round (no receipt, no attempt), remember the dirty set to log once. Test: dirty checkout → attempt count unchanged after a cycle.
- [x] 3.2 `af status` / `af status --json` surfaces `waiting: merge target dirty (<path>)` for parked tasks.
- [x] 3.3 E2E: dirty base parks dispatch → commit → dispatch resumes → task merges.

## 4. Docs & housekeeping

- [x] 4.1 Update `docs/arc42/06-runtime-view.md` (dispatch pre-flight) and `08-crosscutting-concepts.md` (failure classification on agent output) in the same change.
- [x] 4.2 `README.md`: note the `; last output:` suffix on receipts and the dirty-target parking under the `af status` section.
- [x] 4.3 `CHANGELOG.md` entry.
- [x] 4.4 Re-run the receipt forensics script against the archived wo receipts with the new classifier (offline, pure function) and record the before/after `agent`/`infra` split in the changelog entry (expected: ~160 reclassified infra).

## 5. Gates

- [x] 5.1 `cargo fmt --check && cargo clippy --all-targets -- -D warnings`
- [x] 5.2 `cargo test` (full suite, offline)
- [x] 5.3 `./scripts/coverage-gate.sh` — ratchet must not fall; add tests for any new uncovered lines.
