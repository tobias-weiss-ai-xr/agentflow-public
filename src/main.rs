//! The `af` binary: thin CLI over the agentflow library.

use agentflow::config::{self, Settings};
use agentflow::run::{self, RunOptions};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const USAGE: &str = "\
af — parallel LLM task execution on isolated git worktrees

USAGE:
  af run       [--once] [--dry-run] [--worker NAME] [--task ID] [--poll SECS] [--tasks FILE] [--workers FILE]
  af status    [--json]
  af api       status [--json] | results --task ID
  af attach    ID
  af cost      [--task ID] [--last] [--since DATE|UNIX_TS]
  af clean     [--dry-run]
  af stop      signal the running orchestrator and request a drain
  af resume    resume an interrupted run (alias for `af run`)
  af recover   --task ID [--attempt N] [--dry-run]
  af validate  [--worker NAME] [--tasks FILE] [--workers FILE]
  af --help | --version

COST REPORT WINDOWS (af cost):
  --last                aggregate only the most recent receipt per task:
                        the one with the greatest ts, ties broken by the
                        greater attempt number — retries are not
                        double-counted. ATTEMPTS is 1 per task.
  --since DATE|UNIX_TS  aggregate only receipts with ts >= the instant;
                        DATE is YYYY-MM-DD (UTC midnight) or a bare unix
                        timestamp. Applied before --last, so
                        `--last --since D` = the latest attempt per task
                        since D.
  The windows compose with each other and with --task ID (which narrows
  the table rows); the TOTAL line and the per-worker trust block are
  always computed over the selected receipts only. Plain `af cost`
  totals every attempt of every task.

ENV: AF_REPO_DIR, AF_STATE_DIR, AF_MAX_PARALLEL, AF_BRANCH_PREFIX, AF_POLL,
     AF_GATE_ENV, AF_TASKS_JSON, AF_WORKERS_JSON, AF_AGENT_TIMEOUT_S
     (legacy TF_* names still work but are deprecated; use AF_*)";

#[derive(Debug)]
struct Args {
    cmd: String,
    once: bool,
    dry_run: bool,
    detach: bool,
    worker: Option<String>,
    task: Option<String>,
    poll: Option<u64>,
    json: bool,
    tasks_file: Option<PathBuf>,
    workers_file: Option<PathBuf>,
    repos_file: Option<PathBuf>,
    last: bool,
    since: Option<u64>,
    attempt: Option<u32>,
}

fn parse(argv: &[String]) -> Result<Args, String> {
    let mut a = Args {
        cmd: String::new(),
        once: false,
        dry_run: false,
        detach: false,
        worker: None,
        task: None,
        poll: None,
        json: false,
        tasks_file: None,
        workers_file: None,
        repos_file: None,
        last: false,
        since: None,
        attempt: None,
    };
    let mut it = argv.iter();
    a.cmd = it.next().cloned().unwrap_or_default();
    // `resume` is an alias for `run` (P1.8): an explicit "continue from
    // where we left off" entry point. `af run` already resumes stale tasks
    // via the startup heal, but `resume` makes the intent unambiguous.
    if a.cmd == "resume" {
        a.cmd = "run".to_string();
    }
    // `api` collapses into its subcommand: "status" | "results"
    if a.cmd == "api" {
        let sub = it
            .next()
            .ok_or("af api requires a subcommand: status | results")?;
        a.cmd = format!("api-{sub}");
    }
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--once" => a.once = true,
            "--dry-run" => a.dry_run = true,
            "--detach" => a.detach = true,
            "--json" => a.json = true,
            "--worker" => a.worker = Some(it.next().ok_or("--worker needs a value")?.clone()),
            "--task" => a.task = Some(it.next().ok_or("--task needs a value")?.clone()),
            "--poll" => {
                a.poll = Some(
                    it.next()
                        .ok_or("--poll needs a value")?
                        .parse()
                        .map_err(|_| "--poll must be an integer")?,
                )
            }
            "--tasks" => {
                a.tasks_file = Some(PathBuf::from(it.next().ok_or("--tasks needs a value")?))
            }
            "--workers" => {
                a.workers_file = Some(PathBuf::from(it.next().ok_or("--workers needs a value")?))
            }
            "--repos" => {
                a.repos_file = Some(PathBuf::from(it.next().ok_or("--repos needs a value")?))
            }
            "--last" => a.last = true,
            "--attempt" => {
                a.attempt = Some(
                    it.next()
                        .ok_or("--attempt needs a value")?
                        .parse()
                        .map_err(|_| "--attempt must be an integer")?,
                )
            }
            "--since" => {
                let v = it.next().ok_or("--since needs a value")?;
                a.since = Some(run::parse_since(v).map_err(|e| format!("--since: {e}"))?);
            }
            other if other.starts_with('-') => return Err(format!("unknown flag: {other}")),
            other => {
                // bare positional (e.g. `af attach <id>`)
                if a.task.is_none() {
                    a.task = Some(other.to_string());
                } else {
                    return Err(format!("unexpected argument: {other}"));
                }
            }
        }
    }
    Ok(a)
}

/// Crash log (feedback §6.3): a panic in the run loop appends one line to
/// `<state dir>/crash.log` — timestamp, thread, message, location — so an
/// unattended death is diagnosable afterwards. Chains to the default hook:
/// the panic's stderr output is preserved verbatim. Installed only for
/// commands that operate on a state dir (`af run`).
fn install_crash_log(state_dir: &std::path::Path) {
    use std::sync::OnceLock;
    static CRASH_DIR: OnceLock<std::path::PathBuf> = OnceLock::new();
    if CRASH_DIR.set(state_dir.to_path_buf()).is_err() {
        return; // already installed for this process
    }
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        default_hook(info);
        if let Some(dir) = CRASH_DIR.get() {
            let location = info
                .location()
                .map(|l| l.to_string())
                .unwrap_or_else(|| "?".into());
            let msg = info
                .payload()
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| info.payload().downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "non-string panic payload".into());
            let line = format!(
                "{} panic thread={:?} at {location}: {msg}\n",
                agentflow::state::now_ts(),
                std::thread::current().name().unwrap_or("<unnamed>"),
            );
            let _ = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(dir.join("crash.log"))
                .and_then(|mut f| std::io::Write::write_all(&mut f, line.as_bytes()));
        }
    }));
}

fn load_cfg(args: &Args) -> Result<(config::Config, Settings), String> {
    load_cfg_inner(args, false)
}

fn load_cfg_status(args: &Args) -> Result<(config::Config, Settings), String> {
    load_cfg_inner(args, true)
}

fn load_cfg_inner(
    args: &Args,
    allow_missing_tasks: bool,
) -> Result<(config::Config, Settings), String> {
    let st = Settings::from_env();
    let tasks = args
        .tasks_file
        .clone()
        .unwrap_or_else(|| st.tasks_file.clone());
    let workers = args
        .workers_file
        .clone()
        .unwrap_or_else(|| st.workers_file.clone());
    // §6.1: status does not need workers; if the workers file is missing,
    // fall back to an empty worker list so the operator can still inspect.
    let workers = if allow_missing_tasks && !workers.exists() {
        let tmp = st.state_dir.join(".status-workers.json");
        let _ = std::fs::write(
            &tmp,
            r#"{"defaults":{},"workers":[{"name":"_status","provider":"x","model":"x","enabled":true,"cli":"builtin","api_base":"http://localhost"}]}"#,
        );
        tmp
    } else {
        workers
    };
    // Multi-repo (ADR-11): --repos > AF_REPOS_JSON > <tasks dir>/repos.json;
    // missing file = single-repo mode.
    let repos_override = args
        .repos_file
        .clone()
        .or_else(|| config::af_var("AF_REPOS_JSON").map(PathBuf::from));
    // §6.1: `af status` should render from the state dir without requiring a
    // tasks file. When the tasks file is missing, fall back to the task IDs
    // recorded in run-state.json so the operator can inspect a running
    // campaign without supplying AF_TASKS_JSON.
    let cfg = if allow_missing_tasks && !tasks.exists() {
        let rs_path = st.state_dir.join("run-state.json");
        if let Ok(body) = std::fs::read_to_string(&rs_path) {
            let map: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
            let mut ids: Vec<String> = map
                .as_object()
                .map(|o| o.keys().cloned().collect())
                .unwrap_or_default();
            ids.sort();
            // Build a minimal config with empty workers (status doesn't need
            // them) and one task per recorded id.
            let tasks_json = serde_json::json!({
                "tasks": ids.iter().map(|id| serde_json::json!({"id": id, "title": id})).collect::<Vec<_>>()
            });
            let tmp = st.state_dir.join(".status-tasks.json");
            let _ = std::fs::write(&tmp, tasks_json.to_string());
            let cfg = config::load_full(&tmp, &workers, repos_override)?;
            let _ = std::fs::remove_file(&tmp);
            let _ = std::fs::remove_file(st.state_dir.join(".status-workers.json"));
            cfg
        } else {
            config::load_full(&tasks, &workers, repos_override)?
        }
    } else {
        config::load_full(&tasks, &workers, repos_override)?
    };
    let repo_warns = cfg.repo_warnings(&st.repo_dir);
    let mut cfg = cfg;
    cfg.warnings.extend(repo_warns);
    for w in &cfg.warnings {
        eprintln!("warning: {w}");
    }
    Ok((cfg, st))
}

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.is_empty() || argv[0] == "--help" || argv[0] == "-h" {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    if argv[0] == "--version" || argv[0] == "-V" {
        println!(
            "af {}",
            option_env!("AF_BUILD_VERSION").unwrap_or(env!("CARGO_PKG_VERSION"))
        );
        return ExitCode::SUCCESS;
    }
    let args = match parse(&argv) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: {e}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };

    let is_status = args.cmd == "status" || args.cmd == "api-status";
    let (cfg, st) = if is_status {
        match load_cfg_status(&args) {
            Ok(x) => x,
            Err(e) => {
                eprintln!("config error: {e}");
                eprintln!(
                    "  (state dir: {})",
                    std::env::var("AF_STATE_DIR").unwrap_or_else(|_| "state".into())
                );
                return ExitCode::from(2);
            }
        }
    } else {
        match load_cfg(&args) {
            Ok(x) => x,
            Err(e) => {
                eprintln!("config error: {e}");
                return ExitCode::from(2);
            }
        }
    };

    let code = match args.cmd.as_str() {
        "run" => {
            if args.detach {
                // Spawn a detached child process running `af run` (without
                // --detach) with output redirected to a log file in the state
                // dir. Print the pid + state dir so the caller can tail/join.
                let log_path = st.state_dir.join("af-run.log");
                let _ = std::fs::create_dir_all(&st.state_dir);
                let log_file = std::fs::File::create(&log_path).ok();
                let mut cmd = std::process::Command::new(
                    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("af")),
                );
                // Re-run without --detach, inheriting all other args/env.
                cmd.args(std::env::args().skip(1).filter(|a| a != "--detach"));
                if let Some(f) = log_file {
                    use std::process::Stdio;
                    cmd.stdout(Stdio::from(f.try_clone().unwrap()));
                    cmd.stderr(Stdio::from(f));
                    cmd.stdin(Stdio::null());
                }
                match cmd.spawn() {
                    Ok(child) => {
                        println!(
                            "detached: pid {} (state dir: {}, log: {})",
                            child.id(),
                            st.state_dir.display(),
                            log_path.display()
                        );
                        0
                    }
                    Err(e) => {
                        eprintln!("failed to detach: {e}");
                        2
                    }
                }
            } else {
                install_crash_log(&st.state_dir);
                let opts = RunOptions {
                    once: args.once,
                    dry_run: args.dry_run,
                    worker_filter: args.worker.clone(),
                    task_filter: args.task.clone(),
                    poll_secs: args.poll,
                };
                run::run_loop(&cfg, &st, &opts)
            }
        }
        "status" => {
            if args.json {
                println!("{}", run::status_json(&cfg, &st));
            } else {
                println!("{}", run::status_board(&cfg, &st));
            }
            0
        }
        "wait" => {
            // Block until every task in the state dir reaches a terminal state
            // (done or failed), polling every `--poll` seconds (default 15).
            let poll = args.poll.unwrap_or(st.poll_secs).max(1);
            loop {
                let store = agentflow::state::Store::new(st.state_dir.clone());
                let status = store.load();
                let all_terminal = cfg.tasks.iter().all(|t| {
                    status
                        .get(&t.id)
                        .map(|s| s.state.is_terminal())
                        .unwrap_or(false)
                });
                if all_terminal || cfg.tasks.is_empty() {
                    println!("{}", run::status_board(&cfg, &st));
                    break;
                }
                std::thread::sleep(std::time::Duration::from_secs(poll));
            }
            0
        }
        "api-status" => {
            if args.json {
                println!("{}", run::status_json(&cfg, &st));
            } else {
                println!("{}", run::status_board(&cfg, &st));
            }
            0
        }
        "api-results" => {
            let id = match &args.task {
                Some(t) => t.clone(),
                None => {
                    eprintln!("error: af api results requires --task ID");
                    return ExitCode::from(2);
                }
            };
            print!("{}", run::results(&cfg, &st, &id));
            0
        }
        "attach" => {
            let id = match &args.task {
                Some(t) => t.clone(),
                None => {
                    eprintln!("error: af attach requires a task id (af attach <id>)");
                    return ExitCode::from(2);
                }
            };
            run::attach(&st, &id)
        }
        "cost" => {
            let filter = run::CostFilter {
                task: args.task.clone(),
                last: args.last,
                since: args.since,
            };
            println!("{}", run::cost(&cfg, &st, &filter));
            0
        }
        "clean" => run::clean(&cfg, &st, args.dry_run),
        "stop" => run::stop_cmd(&st),
        "recover" => match &args.task {
            Some(id) => run::recover(&cfg, &st, id, args.attempt, args.dry_run),
            None => {
                eprintln!("error: af recover requires --task ID");
                2
            }
        },
        "validate" => {
            // Pre-flight only: load_cfg already loaded + validated the config
            // (hard errors exit 2 before dispatch) and printed its warnings.
            // Nothing is dispatched here — no scheduler, no agents, no state.
            if let Some(name) = &args.worker {
                if !cfg.workers.iter().any(|w| &w.name == name && w.enabled) {
                    eprintln!(
                        "error: unknown or disabled worker '{name}' (known workers: {})",
                        cfg.workers
                            .iter()
                            .map(|w| w.name.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    );
                    return ExitCode::from(2);
                }
            }
            // ADR-16 (code-graph-assisted task scoping): with a declared AND
            // parseable module map, warn when two tasks' scopes are
            // call-connected — semantic contention textual scope globs cannot
            // see. Best-effort: if the map becomes unreadable/invalid between
            // load and here the check silently degrades to a no-op
            // (config::load already warned about the stale declaration).
            let mut cfg = cfg;
            let contention = module_map_contention_warnings(&cfg);
            cfg.warnings.extend(contention.iter().cloned());
            for w in &contention {
                eprintln!("{w}");
            }
            let enabled = cfg.workers.iter().filter(|w| w.enabled).count();
            println!(
                "config OK: {} task(s), {} worker(s), {} enabled, {} warning(s)",
                cfg.tasks.len(),
                cfg.workers.len(),
                enabled,
                cfg.warnings.len()
            );
            0
        }
        other => {
            eprintln!("error: unknown command '{other}'\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    ExitCode::from(code.clamp(0, 255) as u8)
}

// ---------------------------------------------------------------------------
// ADR-16: semantic-contention check (module map consumed by `af validate`)
// ---------------------------------------------------------------------------

/// One directed call edge from the module map (file-level, see
/// docs/module-map.md). Direction matters: `(A → B)` is distinct from
/// `(B → A)`, but the contention check is symmetric over task pairs.
type CallEdge = (String, String);

/// Parse the call edges out of a module map (docs/module-map.md schema:
/// `{"files": [...], "functions": [...], "calls": [{"from","to"}, ...]}`).
/// Only `calls` is consumed here; `files`/`functions` are decorative for
/// validation. Entries missing either endpoint are skipped (a producer
/// bug must not crash a pre-flight check); duplicate edges are collapsed
/// downstream because the contention check treats edges as a set.
fn module_map_call_edges(path: &Path) -> Result<Vec<CallEdge>, String> {
    let body = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let v: serde_json::Value = serde_json::from_str(&body).map_err(|e| e.to_string())?;
    let mut edges = Vec::new();
    if let Some(calls) = v.get("calls").and_then(|c| c.as_array()) {
        for c in calls {
            if let (Some(from), Some(to)) = (
                c.get("from").and_then(|f| f.as_str()),
                c.get("to").and_then(|t| t.as_str()),
            ) {
                edges.push((from.to_string(), to.to_string()));
            }
        }
    }
    Ok(edges)
}

/// True when `file` is allowed by any entry of `scope`. Uses the SAME
/// matcher as admission control and enforcement
/// (`agentflow::scheduler::scope_overlap` — the repo has exactly one glob
/// implementation, see `config::validate`'s touch-coverage rule). An EMPTY
/// `scope` means "any file" (existing semantics), so a scope-less task is
/// call-connected to everything the map knows about.
fn scope_hits(scope: &[String], file: &str) -> bool {
    scope
        .iter()
        .any(|s| agentflow::scheduler::scope_overlap(file, s))
}

/// ADR-16 semantic contention: for every unordered pair of tasks, a call
/// edge `(from → to)` connects them when one task's scope hits `from` and
/// the other's hits `to` (in either direction). Returns one warning line
/// per (pair, edge), sorted for deterministic output (pairs and edges are
/// visited in config order, but a campaign's module map is produced by an
/// external tool whose edge order is not guaranteed).
fn module_map_contention_warnings(cfg: &config::Config) -> Vec<String> {
    let Some(map_path) = &cfg.module_map else {
        return Vec::new();
    };
    let edges = match module_map_call_edges(map_path) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::new();
    for (i, ti) in cfg.tasks.iter().enumerate() {
        for tj in cfg.tasks.iter().skip(i + 1) {
            for (from, to) in &edges {
                let forward = scope_hits(&ti.scope, from) && scope_hits(&tj.scope, to);
                let reverse = scope_hits(&ti.scope, to) && scope_hits(&tj.scope, from);
                if forward || reverse {
                    out.push(format!(
                        "warning: tasks '{}' and '{}' are call-connected via module map \
                         ({} -> {}): consider sequencing with deps",
                        ti.id, tj.id, from, to
                    ));
                }
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_run_flags() {
        let argv = vec![
            "run".to_string(),
            "--once".to_string(),
            "--worker".to_string(),
            "w1".to_string(),
            "--poll".to_string(),
            "3".to_string(),
        ];
        let a = parse(&argv).unwrap();
        assert_eq!(a.cmd, "run");
        assert!(a.once);
        assert_eq!(a.worker.as_deref(), Some("w1"));
        assert_eq!(a.poll, Some(3));
    }

    #[test]
    fn parse_api_subcommands() {
        let a = parse(&[
            "api".to_string(),
            "status".to_string(),
            "--json".to_string(),
        ])
        .unwrap();
        assert_eq!(a.cmd, "api-status");
        assert!(a.json);
        let a = parse(&[
            "api".to_string(),
            "results".to_string(),
            "--task".to_string(),
            "X".to_string(),
        ])
        .unwrap();
        assert_eq!(a.cmd, "api-results");
        assert_eq!(a.task.as_deref(), Some("X"));
    }

    #[test]
    fn parse_rejects_unknown_flag() {
        assert!(parse(&["run".to_string(), "--nope".to_string()]).is_err());
    }

    #[test]
    fn parse_attempt_flag() {
        // `--attempt N` is an optional u32 for `af recover`.
        let a = parse(&[
            "recover".to_string(),
            "--task".to_string(),
            "C".to_string(),
            "--attempt".to_string(),
            "3".to_string(),
        ])
        .unwrap();
        assert_eq!(a.cmd, "recover");
        assert_eq!(a.task.as_deref(), Some("C"));
        assert_eq!(a.attempt, Some(3));
        assert_eq!(parse(&["recover".to_string()]).unwrap().attempt, None);

        // A missing or unparseable value is an error naming the flag.
        let err = parse(&["recover".to_string(), "--attempt".to_string()]).unwrap_err();
        assert_eq!(err, "--attempt needs a value");
        let err = parse(&[
            "recover".to_string(),
            "--attempt".to_string(),
            "nope".to_string(),
        ])
        .unwrap_err();
        assert!(err.starts_with("--attempt"), "{err}");
    }

    #[test]
    fn parse_cost_window_flags() {
        let a = parse(&[
            "cost".to_string(),
            "--last".to_string(),
            "--since".to_string(),
            "2024-01-01".to_string(),
        ])
        .unwrap();
        assert_eq!(a.cmd, "cost");
        assert!(a.last);
        assert_eq!(a.since, Some(1_704_067_200), "YYYY-MM-DD is UTC midnight");

        let a = parse(&[
            "cost".to_string(),
            "--since".to_string(),
            "1700000000".to_string(),
        ])
        .unwrap();
        assert!(!a.last);
        assert_eq!(a.since, Some(1_700_000_000), "bare unix timestamp");
    }

    #[test]
    fn parse_rejects_bad_since_value() {
        // A missing value and an unparseable value are errors naming the
        // flag — surfaced as exit 2 by main(), never a panic.
        assert!(parse(&["cost".to_string(), "--since".to_string()]).is_err());
        let err = parse(&[
            "cost".to_string(),
            "--since".to_string(),
            "not-a-date".to_string(),
        ])
        .unwrap_err();
        assert!(err.starts_with("--since:"), "{err}");
        assert!(err.contains("not-a-date"), "{err}");
    }
    #[test]
    // spec: lifecycle/cooperative-stop-drains-in-flight-attempts#crash-log-captures-an-unexpected-panic
    fn crash_log_captures_a_panic() {
        let dir = std::env::temp_dir().join(format!(
            "af-crash-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        install_crash_log(&dir);
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            panic!("boom-crash-log-probe");
        }));
        let log = std::fs::read_to_string(dir.join("crash.log")).expect("crash.log written");
        assert!(
            log.contains("boom-crash-log-probe"),
            "message captured: {log}"
        );
        assert!(log.contains("panic thread="), "structured line: {log}");
    }
}
