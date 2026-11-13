//! ADR-16 (code-graph-assisted task scoping), validate side: `af validate`
//! consumes the OPTIONAL module map declared at the top level of tasks.json
//! (see docs/module-map.md) and warns when two tasks' scopes are
//! call-connected — semantic contention that textual scope globs cannot see.
//! See docs/arc42/09-architecture-decisions.md (ADR-16).
//!
//! Contract under test here (spawns the real binary, pre-flight only —
//! nothing is dispatched, no state dirs are created):
//! - two tasks with disjoint scopes joined by a call edge (A → B) → exactly
//!   one warning naming both tasks, counted in the `config OK` summary
//! - call-connected files OUTSIDE both tasks' scopes → no warning
//! - the reverse direction (B → A) is symmetric for the task pair
//! - multiple contended pairs print in sorted, deterministic order
//! - no `module_map` declaration → the check is silently off (feature off)

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static N: AtomicUsize = AtomicUsize::new(0);

const WORKERS: &str = r#"{ "workers": [ {"name":"w1","provider":"openai","model":"gpt-4o","params_b":8,"enabled":true} ] }"#;

/// Scratch campaign dir: `config/tasks.json` (+ `config/module_map.json`
/// when `module_map` is Some), `config/workers.json`. Returns the dir.
fn scratch(tasks: &str, module_map: Option<&str>) -> PathBuf {
    let n = N.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("af-mm-validate-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let config_dir = dir.join("config");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(config_dir.join("tasks.json"), tasks).unwrap();
    std::fs::write(config_dir.join("workers.json"), WORKERS).unwrap();
    if let Some(map) = module_map {
        let mut f = std::fs::File::create(config_dir.join("module_map.json")).unwrap();
        f.write_all(map.as_bytes()).unwrap();
    }
    dir
}

/// Two tasks with disjoint textual scopes (`src/a.rs` vs `src/b.rs`) plus a
/// declared module map.
fn two_task_campaign(module_map_calls: &str) -> PathBuf {
    let tasks = r#"{
        "module_map": "module_map.json",
        "tasks": [
            {"id":"A","title":"a","scope":["src/a.rs"],"accept":"true"},
            {"id":"B","title":"b","scope":["src/b.rs"],"accept":"true"}
        ]
    }"#;
    let map = format!(
        r#"{{ "files": ["src/a.rs","src/b.rs"], "functions": [], "calls": {} }}"#,
        module_map_calls
    );
    scratch(tasks, Some(&map))
}

fn af(dir: &Path, args: &[&str]) -> (i32, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_af"))
        .args(args)
        .env("AF_TASKS_JSON", dir.join("config").join("tasks.json"))
        .env("AF_WORKERS_JSON", dir.join("config").join("workers.json"))
        .env("AF_STATE_DIR", dir.join("state"))
        .env("AF_REPO_DIR", dir.join("repo"))
        .env("AF_WORKTREE_ROOT", dir.join("wt"))
        .output()
        .expect("spawn af");
    (
        out.status.code().unwrap_or(-1),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

fn contention_lines(out: &str) -> Vec<&str> {
    out.lines()
        .filter(|l| l.contains("call-connected via module map"))
        .collect()
}

// spec: arc42 ADR-16 — validate warns when two tasks' scopes are call-connected
#[test]
fn call_connected_disjoint_scopes_warn_exactly_once() {
    let dir = two_task_campaign(r#"[{"from":"src/a.rs","to":"src/b.rs"}]"#);
    let (code, out) = af(&dir, &["validate"]);
    assert_eq!(code, 0, "a warning never blocks validation: {out}");

    let lines = contention_lines(&out);
    assert_eq!(
        lines.len(),
        1,
        "exactly one contention warning for one contended pair/edge: {out}"
    );
    assert_eq!(
        lines[0],
        "warning: tasks 'A' and 'B' are call-connected via module map \
         (src/a.rs -> src/b.rs): consider sequencing with deps",
        "warning names both tasks and the connecting edge: {out}"
    );
    assert!(
        out.contains("config OK: 2 task(s), 1 worker(s), 1 enabled, 1 warning(s)"),
        "the contention warning is counted in the summary line: {out}"
    );
    assert!(
        !dir.join("state").exists() && !dir.join("wt").exists(),
        "validate is pre-flight only — no state or worktree dirs"
    );
}

// spec: arc42 ADR-16 — call-connected files outside both scopes stay silent
#[test]
fn unconnected_call_files_outside_both_scopes_stay_silent() {
    let dir = two_task_campaign(r#"[{"from":"docs/x.md","to":"docs/y.md"}]"#);
    let (code, out) = af(&dir, &["validate"]);
    assert_eq!(code, 0, "validate succeeds: {out}");
    assert!(
        contention_lines(&out).is_empty(),
        "no contention warning when no edge touches either scope: {out}"
    );
    assert!(
        out.contains("0 warning(s)"),
        "summary stays at zero warnings: {out}"
    );
}

// spec: arc42 ADR-16 — the pair check is symmetric over edge direction
#[test]
fn reverse_call_direction_also_warns() {
    let dir = two_task_campaign(r#"[{"from":"src/b.rs","to":"src/a.rs"}]"#);
    let (code, out) = af(&dir, &["validate"]);
    assert_eq!(code, 0, "a warning never blocks validation: {out}");
    let lines = contention_lines(&out);
    assert_eq!(
        lines.len(),
        1,
        "exactly one warning for the reversed edge: {out}"
    );
    assert!(
        lines[0].contains("tasks 'A' and 'B'") && lines[0].contains("(src/b.rs -> src/a.rs)"),
        "reversed edge still connects A and B, edge printed in map direction: {out}"
    );
}

// spec: arc42 ADR-16 — multiple contended pairs print deterministically
#[test]
fn multiple_contended_pairs_print_sorted() {
    let tasks = r#"{
        "module_map": "module_map.json",
        "tasks": [
            {"id":"A","title":"a","scope":["src/a.rs"],"accept":"true"},
            {"id":"B","title":"b","scope":["src/b.rs"],"accept":"true"},
            {"id":"C","title":"c","scope":["src/c.rs"],"accept":"true"}
        ]
    }"#;
    let map = r#"{ "files": [], "functions": [], "calls": [
        {"from":"src/c.rs","to":"src/a.rs"},
        {"from":"src/a.rs","to":"src/b.rs"}
    ] }"#;
    let dir = scratch(tasks, Some(map));
    let (code, out) = af(&dir, &["validate"]);
    assert_eq!(code, 0, "a warning never blocks validation: {out}");
    let lines = contention_lines(&out);
    assert_eq!(lines.len(), 2, "one warning per contended pair/edge: {out}");
    assert_eq!(
        lines,
        vec![
            "warning: tasks 'A' and 'B' are call-connected via module map \
             (src/a.rs -> src/b.rs): consider sequencing with deps",
            "warning: tasks 'A' and 'C' are call-connected via module map \
             (src/c.rs -> src/a.rs): consider sequencing with deps",
        ],
        "output is sorted (deterministic regardless of map edge order): {out}"
    );
}

// spec: arc42 ADR-16 — absent declaration is a silent feature-off
#[test]
fn absent_module_map_declaration_stays_silent() {
    let tasks = r#"{
        "tasks": [
            {"id":"A","title":"a","scope":["src/a.rs"],"accept":"true"},
            {"id":"B","title":"b","scope":["src/b.rs"],"accept":"true"}
        ]
    }"#;
    let dir = scratch(tasks, None);
    let (code, out) = af(&dir, &["validate"]);
    assert_eq!(code, 0, "validate succeeds: {out}");
    assert!(contention_lines(&out).is_empty(), "feature off: {out}");
    assert!(out.contains("0 warning(s)"), "no module_map noise: {out}");
}
