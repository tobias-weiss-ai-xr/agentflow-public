//! Status commands: human board, JSON status, and `af stop`.

use crate::config::{Config, Settings};
use crate::state::{Store, TaskStatus};
use std::collections::HashMap;

pub(crate) fn board_of(cfg: &Config, status: &HashMap<String, TaskStatus>) -> String {
    let mut rows = vec![format!(
        "{:<12} {:<9} {:<8} {}",
        "TASK", "STATE", "ATTEMPTS", "LAST ERROR"
    )];
    for t in &cfg.tasks {
        let s = status.get(&t.id).cloned().unwrap_or_default();
        let err = s
            .last_error
            .as_deref()
            .map(|e| e.chars().take(48).collect::<String>()) // chars, not bytes: UTF-8 safe
            .unwrap_or_default();
        rows.push(format!(
            "{:<12} {:<9?} {:<8} {}",
            t.id, s.state, s.attempts, err
        ));
    }
    rows.join("\n")
}

/// `af status` / `af api status` (human board).
///
/// The status file is loaded loudly by [`Store::load`] already; receipts are
/// not needed for the board but ARE loaded through the checked loader so a
/// torn `*.json` receipt surfaces here too (one `warning:` line per file)
/// instead of staying invisible. History that cannot be parsed never blocks
/// the command.
pub fn status_board(cfg: &Config, st: &Settings) -> String {
    let store = Store::new(st.state_dir.clone());
    let status = store.load();
    let (_, problems) = store.load_receipts_checked();
    let mut out = String::new();
    // Name the consulted state dir first (feedback §6.1): a board without it
    // invites "which campaign am I even looking at" — the §6.1 report itself
    // was born from exactly that confusion.
    out.push_str(&format!("state dir: {}\n", st.state_dir.display()));
    for p in &problems {
        out.push_str(&format!("warning: {p}\n"));
    }
    out.push_str(&board_of(cfg, &status));
    out
}

/// `af stop` (feedback §6.4): create the stop flag (the running orchestrator
/// drains on its next poll) and signal the recorded pid as a best-effort
/// early termination. Liveness is a `/proc/<pid>` existence check (Linux
/// dev/CI hosts); a stale or missing pid file is a warning, never fatal.
/// Exit code: 0 — the stop wish is always recorded.
pub fn stop_cmd(st: &Settings) -> i32 {
    let flag = st.state_dir.join("stop");
    std::fs::create_dir_all(&st.state_dir).ok();
    match std::fs::write(&flag, format!("{}\n", crate::state::now_ts())) {
        Ok(_) => println!(
            "stop flag written to {}; a running af will drain on its next poll",
            flag.display()
        ),
        Err(e) => eprintln!("warning: could not write stop flag: {e}"),
    }
    let pid_path = st.state_dir.join("af.pid");
    let pid = std::fs::read_to_string(&pid_path)
        .ok()
        .and_then(|s| s.trim().parse::<i32>().ok());
    match pid {
        Some(p) if std::path::Path::new(&format!("/proc/{p}")).exists() => {
            let _ = std::process::Command::new("kill")
                .arg("-TERM")
                .arg(p.to_string())
                .status();
            println!("signaled af pid {p}");
        }
        Some(p) => println!("warning: pid file names dead process {p} (stale from a kill -9?)"),
        None => println!(
            "warning: no readable pid file at {} — nothing to signal",
            pid_path.display()
        ),
    }
    0
}

/// `af api status --json`.
pub fn status_json(cfg: &Config, st: &Settings) -> String {
    let status = Store::new(st.state_dir.clone()).load();
    let receipts = Store::new(st.state_dir.clone()).load_receipts();
    let mut map = serde_json::Map::new();
    for t in &cfg.tasks {
        let s = status.get(&t.id).cloned().unwrap_or_default();
        // Latest surviving receipt for this task (last winning attempt).
        let latest = receipts
            .iter()
            .rev()
            .find(|r| r.task == t.id && r.counts_as_verdict());
        let verdict = latest.map(|r| r.outcome.as_str()).unwrap_or("none");
        let contracts = latest
            .and_then(|r| r.contracts.as_ref())
            .map(|c| {
                c.iter()
                    .map(|v| serde_json::json!({ "contract": v.contract, "verdict": v.verdict }))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        // Failure class of the latest FAILED receipt for this task (the
        // most recent `failed`/`infra` attempt), surfaced so an operator can
        // see whether the last failure was the worker's (`agent`) or the
        // infrastructure's (`infra`). `None` when the task has no failed
        // receipt.
        let failure_class = receipts
            .iter()
            .rev()
            .find(|r| r.task == t.id && r.outcome == "failed")
            .and_then(|r| r.failure_class.clone());
        map.insert(
            t.id.clone(),
            serde_json::json!({
                "id": t.id,
                "state": format!("{:?}", s.state).to_lowercase(),
                "attempts": s.attempts,
                "last_error": s.last_error,
                "verify": t.verify,
                "verdict": verdict,
                "contracts": contracts,
                "failure_class": failure_class,
            }),
        );
    }
    serde_json::to_string_pretty(&serde_json::Value::Object(map)).unwrap_or_default()
}
