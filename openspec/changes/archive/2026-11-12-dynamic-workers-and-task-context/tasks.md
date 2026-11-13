# Tasks: dynamic-workers-and-task-context

TDD order: config field first (RED on missing config support), then prompt
rendering, then pool capacity; every new requirement/scenario gets its
`// spec:` marker (see `tests/spec_traceability.rs`); docs last. Commands:
`cargo test contract_prompt`, `cargo test config::tests`, `cargo test
run::tests`, then full gates in section 6.

## 1. Config: `context` field + `AF_PROMPT_FILE`

- [x] 1.1 `Task.context: Option<Context>` with `Context` an untagged
      `String | Vec<String>` (array joins with `\n`); `#[serde(default)]`;
      normalize empty string / empty array / null → `None`. Doc on the
      struct field (per-task context → `{{CONTEXT}}`).
- [x] 1.2 load tests (RED first): string parses; array joins; empty
      string / empty array / absent → `None`; no regression for existing
      tasks. Markers:
      - `// spec: config/task-schema-loading#task-context-parses-as-a-string`
      - `// spec: config/task-schema-loading#task-context-parses-as-an-array`
      - `// spec: config/task-schema-loading#empty-or-absent-context-is-a-no-op`
- [x] 1.3 `Settings.prompt_file` from `env_or("AF_PROMPT_FILE",
      "prompts/worker.md")` (inherits the `AF_*` → legacy `TF_*` fallback +
      one-time deprecation notice). Unit tests: `AF_PROMPT_FILE` wins;
      `TF_PROMPT_FILE` fallback warns; default. Marker:
      - `// spec: config/environment-overrides#prompt-template-is-selectable-per-campaign`

## 2. Prompt rendering: `{{CONTEXT}}`, `{{PREVIOUS_ERROR}}`, conditional markers

- [x] 2.1 `render_prompt` (RED first): substitute `{{CONTEXT}}` (empty when
      `None`); substitute `{{PREVIOUS_ERROR}}` with the failure-context block
      when present (keep the append-at-end fallback for templates without the
      token; attempt 1 renders the token empty); when a custom template omits
      `{{CONTEXT}}` and the task declares context, append a `## Task context`
      section + warn.
- [x] 2.2 generalize the conditional-marker strip: drop
      `{{#CONTEXT}}…{{/CONTEXT}}` as a whole SECTION when the task has no
      context; otherwise strip the markers line-wise like `{{#SCOPE}}`.
- [x] 2.3 `tests/contract_prompt.rs` scenarios (markers):
      - `// spec: lifecycle/prompt-rendering#task-context-is-injected`
      - `// spec: lifecycle/prompt-rendering#conditional-context-section-vanishes-when-unset`
      - `// spec: lifecycle/prompt-rendering#a-task-context-template-that-omits-the-placeholder-is-repaired`
      - `// spec: lifecycle/prompt-rendering#first-attempt-has-no-history-block`
      - `// spec: lifecycle/prompt-rendering#custom-template-is-selected-per-campaign`
- [x] 2.4 `cargo test contract_prompt` green.

## 3. Pool capacity follows the live enabled set

- [x] 3.1 extract pure `pool_capacity(&Settings, &Config) -> usize`:
      `AF_MAX_PARALLEL == 0` → current enabled-worker count (clamped to ≥ 1);
      else the fixed ceiling. Unit tests (RED first):
      - `// spec: scheduling/worker-pool-tracks-the-live-enabled-set`
      - `// spec: scheduling/worker-pool-tracks-the-live-enabled-set#default-ceiling-follows-the-live-worker-count`
      - `// spec: scheduling/worker-pool-tracks-the-live-enabled-set#explicit-ceiling-is-never-recomputed`
- [x] 3.2 `run_loop`: make `max_parallel` mutable, seed from
      `pool_capacity(st, &cfg)`, and recompute in the hot-reload branch
      (after `cfg = fresh; ctx.cfg = cfg.clone();`); log the new value when
      it changes. Extend the `hot_reload_*` test family with a worker-set
      change: adding a worker grows capacity, disabling one shrinks it, the
      all-disabled edit keeps the last good config + warns. Markers:
      - `// spec: lifecycle/dispatch-rounds-read-the-current-task-config#adding-a-worker-mid-run-grows-the-pool`
      - `// spec: lifecycle/dispatch-rounds-read-the-current-task-config#disabling-a-worker-mid-run-shrinks-the-pool`
      - `// spec: lifecycle/dispatch-rounds-read-the-current-task-config#an-explicit-ceiling-stays-fixed`
      - `// spec: scheduling/worker-pool-tracks-the-live-enabled-set#disabling-a-worker-reduces-the-pool`
      - `// spec: scheduling/worker-pool-tracks-the-live-enabled-set#all-disabled-reload-keeps-the-last-good-pool`
      (the last two share the config reload test — see `src/config.rs:686` /
      `run.rs` hot-reload behavior; the `config/…#disabling-every-worker…`
      scenario is covered by the same test.)
- [x] 3.3 `cargo test run::tests` green.

## 4. Template cleanup (`prompts/worker.md`)

- [x] 4.1 Reword: delete `{{EPISODES}}` and `{{MISSION}}`; keep and wire
      `{{PREVIOUS_ERROR}}`; add a `{{#CONTEXT}}…{{/CONTEXT}}` "Craft context"
      section; keep `{{SCOPE}}`, `{{ACCEPTANCE}}`, `{{ACCEPT_CMD}}`.
- [x] 4.2 `cargo test contract_prompt` still green (every-render-path
      contract: scope + gate command always present).

## 5. Docs

- [x] 5.1 README: task-schema `context` row (string or array → injected via
      `{{CONTEXT}}`); `AF_PROMPT_FILE` row in the env table (default
      `prompts/worker.md`).
- [x] 5.2 Update `docs/arc42/` runtime-view chapter wherever it names the
      prompt placeholder set or the parallelism derivation (grep for
      `{{SCOPE}}` and `max_parallel`).

## 6. Gates + archive

- [x] 6.1 `cargo fmt --check`
- [x] 6.2 `cargo clippy --all-targets -- -D warnings`
- [x] 6.3 full `cargo test` green (incl. `tests/spec_traceability.rs` —
      every marker resolves once specs are merged)
- [x] 6.4 `scripts/coverage-gate.sh` green (add tests for any newly
      uncovered lines — never lower the floor)
- [x] 6.5 `openspec validate --all` green
- [x] 6.6 archive the change (`openspec archive`, specs merged);
      `cargo test` still green after merge
