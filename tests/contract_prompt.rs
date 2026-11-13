//! Contract tests pinning the prompt-rendering guarantees.
//!
//! Commit f961ffe removed a stray foreign template whose placeholders did
//! not match agentflow's — agents were silently dispatched with NO file
//! scope and NO acceptance gate. These tests pin the CONTRACT so that class
//! of bug can never return silently: whatever render path is taken (a
//! missing `Settings.prompt_file` ⇒ built-in `DEFAULT_PROMPT`, or a present
//! custom template — friendly or hostile), the rendered prompt MUST carry
//! the task's exact gate command, every scope path, the id/title, and no
//! template punctuation may leak through.

use agentflow::config::{Settings, Task, Worker};
use agentflow::execute::render_prompt;
use std::path::{Path, PathBuf};

/// Distinctive gate command every rendered prompt must embed VERBATIM.
const ACCEPT_CMD: &str = "cargo test --test contract_prompt";
const TASK_ID: &str = "r4-contract";
const TASK_TITLE: &str = "pin the prompt rendering contract";
const SCOPE_PATHS: [&str; 2] = ["src/one.rs", "src/two.rs"];

fn contract_task() -> Task {
    Task {
        id: TASK_ID.into(),
        title: TASK_TITLE.into(),
        scope: SCOPE_PATHS.iter().map(|s| s.to_string()).collect(),
        accept: Some(ACCEPT_CMD.into()),
        acceptance_prose: Some("the gate command runs verbatim".into()),
        ..Default::default()
    }
}

fn contract_worker() -> Worker {
    Worker {
        name: "w".into(),
        provider: "zai".into(),
        model: "glm-5.2".into(),
        output: "text".into(),
        args: Vec::new(),
        params_b: None,
        price_per_mtok_usd: None,
        ..Default::default()
    }
}

/// Settings pointed at a specific prompt file. `from_env()` fields other
/// than `prompt_file` are irrelevant to rendering; this mirrors the shape
/// used by execute.rs's own unit tests.
fn settings_with_prompt_file(prompt_file: PathBuf) -> Settings {
    let mut st = Settings::from_env();
    st.prompt_file = prompt_file;
    st
}

fn tmpdir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("af-prompt-contract-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn write_template(dir: &Path, name: &str, body: &str) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, body).unwrap();
    p
}

/// The four prompt guarantees, asserted on one rendered output.
///
/// 1. the task's EXACT `accept` command is present;
/// 2. every scope path is present;
/// 3. the task id and title are present;
/// 4. no unresolved placeholder or conditional marker leaks — a single
///    `{{` ban covers `{{SCOPE}}`, `{{ACCEPT_CMD}}`, `{{#SCOPE}}`, … and
///    any foreign template's unsubstituted tokens alike.
fn assert_prompt_contract(path: &str, out: &str, scope_paths: &[&str]) {
    assert!(
        out.contains(ACCEPT_CMD),
        "{path}: must embed the task's EXACT accept command `{ACCEPT_CMD}`"
    );
    for &p in scope_paths {
        assert!(out.contains(p), "{path}: must contain scope path {p}");
    }
    assert!(out.contains(TASK_ID), "{path}: task id missing");
    assert!(out.contains(TASK_TITLE), "{path}: task title missing");
    assert!(
        !out.contains("{{"),
        "{path}: unresolved placeholder or conditional marker leaked:\n---\n{out}\n---"
    );
}

// spec: lifecycle/prompt-placeholder-guarantee
// spec: lifecycle/prompt-placeholder-guarantee#every-render-path-carries-scope-and-gate-command
// spec: lifecycle/prompt-placeholder-guarantee#no-placeholder-leaks
// spec: lifecycle/prompt-placeholder-guarantee#empty-scope-renders-the-wildcard
#[test]
fn every_render_path_contains_scope_and_accept_cmd() {
    let worker = contract_worker();
    let task = contract_task();
    let empty_scope_task = Task {
        scope: vec![],
        ..contract_task()
    };

    // (a) MISSING template file ⇒ built-in DEFAULT_PROMPT.
    let missing = settings_with_prompt_file(PathBuf::from("no-such-contract-template.md"));
    assert_prompt_contract(
        "missing template (built-in DEFAULT_PROMPT)",
        &render_prompt(&missing, &task, &worker, None),
        &SCOPE_PATHS,
    );

    let dir = tmpdir("paths");

    // (b) PRESENT custom template carrying every supported placeholder.
    let full = write_template(
        &dir,
        "full.md",
        "TASK {{TASK_ID}} — {{TASK_TITLE}}
Files you are allowed to modify:
{{SCOPE}}
Acceptance criteria:
{{ACCEPTANCE}}
Acceptance gate command (run verbatim to verify this task):
{{ACCEPT_CMD}}
You run as {{MODEL}} on {{PROVIDER}}.
",
    );
    assert_prompt_contract(
        "full custom template",
        &render_prompt(
            &settings_with_prompt_file(full.clone()),
            &task,
            &worker,
            None,
        ),
        &SCOPE_PATHS,
    );

    // (c) HOSTILE custom template (the f961ffe shape): omits {{SCOPE}} and
    // {{ACCEPT_CMD}} entirely — the hardening path must still append them.
    let hostile = write_template(
        &dir,
        "hostile.md",
        "Task {{TASK_ID}}: {{TASK_TITLE}} — go do the thing.\n",
    );
    assert_prompt_contract(
        "hostile template (hardening appends scope + gate)",
        &render_prompt(
            &settings_with_prompt_file(hostile.clone()),
            &task,
            &worker,
            None,
        ),
        &SCOPE_PATHS,
    );

    // (d) EMPTY scope list: renders the `*` wildcard, still no leak, still
    // the exact gate command — through every template path.
    for (path, file) in [
        ("empty scope, DEFAULT_PROMPT", None),
        ("empty scope, full template", Some(&full)),
        ("empty scope, hostile template", Some(&hostile)),
    ] {
        let st = settings_with_prompt_file(match file {
            Some(f) => f.clone(),
            None => PathBuf::from("no-such-contract-template.md"),
        });
        let out = render_prompt(&st, &empty_scope_task, &worker, None);
        assert_prompt_contract(path, &out, &[]);
        assert!(
            out.contains('*'),
            "{path}: empty scope must render the `*` wildcard"
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

/// The prompt-rendering scenario in its own words (lifecycle spec): a task
/// with a title and acceptance prose dispatches, and the rendered prompt
/// file contains BOTH — through the built-in default template and a custom
/// template that carries `{{ACCEPTANCE}}`.
// spec: lifecycle/prompt-rendering
// spec: lifecycle/prompt-rendering#template-substitution
#[test]
fn rendered_prompt_carries_title_and_acceptance_prose() {
    const PROSE: &str = "the gate command runs verbatim";
    let worker = contract_worker();
    let task = contract_task(); // title + acceptance_prose = PROSE
    assert_eq!(task.acceptance_prose.as_deref(), Some(PROSE));

    // (a) Missing template file ⇒ built-in DEFAULT_PROMPT.
    let missing = settings_with_prompt_file(PathBuf::from("no-such-contract-template.md"));
    let out = render_prompt(&missing, &task, &worker, None);
    assert!(out.contains(TASK_TITLE), "title substituted: {out}");
    assert!(out.contains(PROSE), "acceptance prose substituted: {out}");

    // (b) Custom template with the ACCEPTANCE placeholder.
    let dir = tmpdir("substitution");
    let full = write_template(
        &dir,
        "full.md",
        "TASK {{TASK_ID}} — {{TASK_TITLE}}\n{{ACCEPTANCE}}\n{{ACCEPT_CMD}}\n{{SCOPE}}\n",
    );
    let out = render_prompt(
        &settings_with_prompt_file(full.clone()),
        &task,
        &worker,
        None,
    );
    assert!(out.contains(TASK_TITLE), "title substituted: {out}");
    assert!(out.contains(PROSE), "acceptance prose substituted: {out}");
    assert!(!out.contains("{{"), "no placeholder leaks: {out}");

    let _ = std::fs::remove_dir_all(&dir);
}

// spec: lifecycle/prompt-placeholder-guarantee#hostile-template-is-repaired
#[test]
fn hostile_template_is_reported_and_repaired() {
    let worker = contract_worker();
    let task = contract_task();
    let dir = tmpdir("hostile-repair");

    // The hostile template (f961ffe failure mode): no {{SCOPE}}, no
    // {{ACCEPTANCE}}, no {{ACCEPT_CMD}}.
    let hostile = write_template(
        &dir,
        "hostile.md",
        "Task {{TASK_ID}}: {{TASK_TITLE}} — go do the thing.\n",
    );
    let out = render_prompt(&settings_with_prompt_file(hostile), &task, &worker, None);

    // Reported + repaired: the omission is flagged by render_prompt on
    // stderr and repaired by appending explicit "(auto-appended)" sections.
    assert!(
        out.contains("## File scope (auto-appended)"),
        "hostile template must be repaired with an auto-appended scope section:\n{out}"
    );
    assert!(
        out.contains("## Acceptance criteria (auto-appended)"),
        "hostile template must be repaired with an auto-appended acceptance section:\n{out}"
    );
    assert!(
        out.contains("## Acceptance gate command (auto-appended)"),
        "hostile template must be repaired with an auto-appended gate command:\n{out}"
    );
    // The repaired sections carry the real payload, verbatim.
    assert!(out.contains(&format!(
        "## File scope (auto-appended)\n{}",
        SCOPE_PATHS.join("\n")
    )));
    assert!(out.contains(&format!(
        "## Acceptance gate command (auto-appended)\n{ACCEPT_CMD}"
    )));
    // The template's own body is still rendered (id/title substituted).
    assert!(out.contains(&format!("Task {TASK_ID}: {TASK_TITLE}")));

    // The repair targets the HOSTILE case specifically: a template that
    // carries every placeholder is passed through with nothing appended.
    let full = write_template(
        &dir,
        "full.md",
        "{{TASK_ID}} {{TASK_TITLE}} {{SCOPE}} {{ACCEPTANCE}} {{ACCEPT_CMD}} {{MODEL}} {{PROVIDER}}\n",
    );
    let out = render_prompt(&settings_with_prompt_file(full), &task, &worker, None);
    assert!(
        !out.contains("(auto-appended)"),
        "well-formed template must not be repaired:\n{out}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// The SHIPPED `prompts/worker.md` template — the file production actually
/// renders — carries no dead placeholders and never leaks `{{`; a crafted
/// task context lands in its `{{#CONTEXT}}` section, and the
/// `{{PREVIOUS_ERROR}}` token holds the retry block.
fn shipped_worker_md() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("prompts/worker.md")
}

// spec: lifecycle/prompt-placeholder-guarantee#no-placeholder-leaks
// spec: lifecycle/prompt-rendering#task-context-is-injected
// spec: lifecycle/prompt-rendering#custom-template-is-selected-per-campaign
#[test]
fn shipped_worker_md_renders_clean_with_context_and_retry_block() {
    use agentflow::config::TaskContext;
    let worker = contract_worker();
    let dir = tmpdir("shipped");
    let template = write_template(
        &dir,
        "copy.md",
        &std::fs::read_to_string(shipped_worker_md()).expect("prompts/worker.md present"),
    );

    // (a) No context, first attempt: full contract, zero placeholder leaks,
    // no dead {{EPISODES}}/{{MISSION}}/{{PREVIOUS_ERROR}} tokens.
    let out = render_prompt(
        &settings_with_prompt_file(template.clone()),
        &contract_task(),
        &worker,
        None,
    );
    assert_prompt_contract("shipped worker.md (first attempt)", &out, &SCOPE_PATHS);
    assert!(
        !out.contains("Previous attempts"),
        "attempt 1 must have no history block"
    );
    for dead in ["{{EPISODES}}", "{{MISSION}}", "{{PREVIOUS_ERROR}}"] {
        assert!(!out.contains(dead), "dead placeholder {dead} must not leak");
    }
    assert!(
        !out.contains("Task context"),
        "no {{#CONTEXT}} section without a declared context"
    );

    // (b) With a declared context: the section appears with the payload.
    let ctx_task = agentflow::config::Task {
        context: Some(TaskContext::Text("read docs/CONVENTIONS.md first".into())),
        ..contract_task()
    };
    let out = render_prompt(
        &settings_with_prompt_file(template.clone()),
        &ctx_task,
        &worker,
        None,
    );
    assert!(
        out.contains("## Task context") && out.contains("read docs/CONVENTIONS.md first"),
        "crafted context must land in the shipped template:\n{out}"
    );
    assert!(
        !out.contains("{{#CONTEXT}}"),
        "marker must not leak when active"
    );

    // (c) Retry: the block substitutes INTO the {{PREVIOUS_ERROR}} token.
    let block = "\n## Previous attempts\n- attempt 1: gate boom\n";
    let out = render_prompt(
        &settings_with_prompt_file(template),
        &contract_task(),
        &worker,
        Some(block),
    );
    assert!(out.contains("attempt 1: gate boom"), "retry block present");
    assert!(
        !out.contains("{{PREVIOUS_ERROR}}"),
        "token must not leak after substitution"
    );
    assert!(
        out.contains("## If you are a merge-conflict retry"),
        "template body intact"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
