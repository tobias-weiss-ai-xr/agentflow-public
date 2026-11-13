//! ADR-16 (code-graph-assisted task scoping): config-side parsing of the
//! OPTIONAL `module_map` declaration at the top level of tasks.json
//! (sibling of `_meta`/`tasks`). See docs/arc42/09-architecture-decisions.md.
//!
//! Contract under test here:
//! - absent field      -> `Config.module_map` is None, no module_map warning
//!   (feature off is SILENT)
//! - declared + valid  -> `Some(path)` with a relative declaration resolved
//!   against the tasks file's parent directory
//! - declared + unreadable/invalid JSON -> None + a non-fatal warning
//!   ("scope-contention checks disabled"), never a hard load error
//! - absolute declaration passes through untouched
//!
//! (The call-connected analysis that CONSUMES the map lives elsewhere.)

use agentflow::config;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

static N: AtomicUsize = AtomicUsize::new(0);

/// Fresh scratch dir under temp_dir (unique per call) holding a minimal
/// valid tasks.json + workers.json; returns the dir and the tasks path.
fn scratch() -> (PathBuf, PathBuf) {
    let n = N.fetch_add(1, Ordering::Relaxed);
    let d = std::env::temp_dir().join(format!("af-mm-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let tasks = d.join("tasks.json");
    std::fs::write(
        &tasks,
        r#"{ "tasks": [ {"id":"T1","title":"x","accept":"true"} ] }"#,
    )
    .unwrap();
    std::fs::write(
        d.join("workers.json"),
        r#"{ "workers": [ {"name":"w1","provider":"openai","model":"gpt-4o","enabled":true} ] }"#,
    )
    .unwrap();
    (d, tasks)
}

#[test]
fn module_map_absent_is_silent() {
    let (_d, tasks) = scratch();
    let cfg = config::load(&tasks, &_d.join("workers.json")).expect("config loads");
    assert!(
        cfg.module_map.is_none(),
        "undeclared module_map must be None"
    );
    assert!(
        !cfg.warnings.iter().any(|w| w.contains("module_map")),
        "absence is a silent feature-off, got warnings: {:?}",
        cfg.warnings
    );
}

#[test]
fn module_map_declared_valid_resolves_against_tasks_dir() {
    let (d, tasks) = scratch();
    std::fs::write(d.join("module_map.json"), r#"{ "modules": [] }"#).unwrap();
    std::fs::write(
        &tasks,
        r#"{ "module_map": "module_map.json", "tasks": [ {"id":"T1","title":"x","accept":"true"} ] }"#,
    )
    .unwrap();

    let cfg = config::load(&tasks, &d.join("workers.json")).expect("config loads");
    assert_eq!(
        cfg.module_map,
        Some(d.join("module_map.json")),
        "relative declaration resolves against the tasks file's parent dir"
    );
}

#[test]
fn module_map_absolute_declaration_passes_through() {
    let (d, tasks) = scratch();
    let abs = d.join("elsewhere.json");
    std::fs::write(&abs, r#"{}"#).unwrap();
    let tasks_json = format!(
        r#"{{ "module_map": "{}", "tasks": [ {{"id":"T1","title":"x","accept":"true"}} ] }}"#,
        abs.display()
    );
    std::fs::write(&tasks, tasks_json).unwrap();

    let cfg = config::load(&tasks, &d.join("workers.json")).expect("config loads");
    assert_eq!(cfg.module_map, Some(abs.clone()));
    assert!(
        !cfg.warnings.iter().any(|w| w.contains("module_map")),
        "a readable valid map must not warn: {:?}",
        cfg.warnings
    );
}

#[test]
fn module_map_missing_file_warns_and_disables() {
    let (d, tasks) = scratch();
    std::fs::write(
        &tasks,
        r#"{ "module_map": "no-such-map.json", "tasks": [ {"id":"T1","title":"x","accept":"true"} ] }"#,
    )
    .unwrap();

    let cfg = config::load(&tasks, &d.join("workers.json")).expect("stale map is NOT a hard error");
    assert_eq!(cfg.module_map, None, "unreadable map degrades to None");
    let w = cfg
        .warnings
        .iter()
        .find(|w| w.contains("module_map"))
        .expect("warning pushed");
    assert!(
        w.contains("scope-contention checks disabled"),
        "warning must say checks are disabled: {w}"
    );
    assert!(
        w.contains("no-such-map.json"),
        "warning must name the offending path: {w}"
    );
}

#[test]
fn module_map_invalid_json_warns_and_disables() {
    let (d, tasks) = scratch();
    std::fs::write(d.join("broken.json"), "not json at all").unwrap();
    std::fs::write(
        &tasks,
        r#"{ "module_map": "broken.json", "tasks": [ {"id":"T1","title":"x","accept":"true"} ] }"#,
    )
    .unwrap();

    let cfg =
        config::load(&tasks, &d.join("workers.json")).expect("invalid map is NOT a hard error");
    assert_eq!(cfg.module_map, None, "unparsable map degrades to None");
    assert!(
        cfg.warnings
            .iter()
            .any(|w| w.contains("module_map") && w.contains("scope-contention checks disabled")),
        "warning pushed for invalid map: {:?}",
        cfg.warnings
    );
    // And the tasks themselves still load fine alongside the stale map.
    assert_eq!(cfg.tasks.len(), 1);
}
