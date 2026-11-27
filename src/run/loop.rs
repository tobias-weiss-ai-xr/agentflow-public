//! Dispatch-loop internals: backoff, candidate reaping, worker pickers,
//! hot reload and the main `run_loop` orchestrator loop.

use super::cost::{
    consumed_tokens, consumed_tokens_for_task, consumed_wall_clock_s, token_budget_exhausted,
    wall_clock_budget_exhausted,
};
use super::dry_run::dry_run;
use super::recover::heal_stale_attempt;
use super::reuse::{reuse_before_dispatch, reuse_disabled};
use super::status::board_of;
use crate::config::{Config, Settings, TaskState};
use crate::execute::{self, ExecCtx, Outcome};
use crate::router::Router;
use crate::scheduler;
use crate::state::{Receipt, Store, TaskStatus};
use crate::worktree;
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

#[derive(Debug, Clone, Default)]
pub struct RunOptions {
    pub once: bool,
    pub dry_run: bool,
    pub worker_filter: Option<String>,
    pub task_filter: Option<String>,
    pub poll_secs: Option<u64>,
}

/// Escalating per-worker backoff state. `level` is the number of
/// consecutive infra failures; `until` is when the cooldown expires.
/// Any verdict (merged/verified/non-infra failed) resets `level` to 0
/// and clears the entry. See `backoff_secs` for the schedule.
#[derive(Clone, Debug)]
pub struct BackoffState {
    pub until: std::time::Instant,
    pub level: u32,
}

/// Cooldown seconds for a worker at `level` consecutive infra failures.
/// Level 1 → 30 s, 2 → 60 s, 3 → 120 s, 4 → 240 s, 5+ → 300 s ceiling.
/// When the pool has exactly one enabled worker, the cooldown is capped
/// at 30 s so a solo campaign is never locked out for minutes. Pure for
/// testing.
pub(crate) fn backoff_secs(level: u32, enabled_workers: usize) -> u64 {
    if level == 0 {
        return 0;
    }
    let raw = 30u64.checked_shl(level.saturating_sub(1)).unwrap_or(300);
    let secs = raw.min(300);
    if enabled_workers <= 1 {
        secs.min(30)
    } else {
        secs
    }
}

/// Fingerprint of a gate-failure reason: `(exit_code, whitespace-
/// normalized tail)`. Returns `None` for non-gate errors. Two gate
/// failures with the same fingerprint are byte-identical gate output
/// (same exit code, same failing tail after whitespace normalization),
/// which is a strong signal of a task-design defect rather than a flake.
/// Pure for testing.
pub(crate) fn gate_fingerprint(error: &str) -> Option<(String, String)> {
    const PREFIX: &str = "acceptance gate failed";
    if !error.starts_with(PREFIX) {
        return None;
    }
    let rest = &error[PREFIX.len()..]; // " (exit N): <tail>"
    let exit_code = rest
        .strip_prefix(" (exit ")
        .and_then(|s| s.split(')').next())
        .unwrap_or("-")
        .to_string();
    let tail = rest.find(": ").map(|i| &rest[i + 2..]).unwrap_or("");
    // Whitespace-normalize the tail so trivial formatting differences
    // (extra spaces, trailing whitespace) don't mask a real defect.
    let normalized = tail.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        return None;
    }
    Some((exit_code, normalized))
}

/// Whether the current gate failure is a repeated identical defect: the
/// previous AGENT-driven gate-failed attempt of the same task has the same
/// fingerprint. Gate-only reruns (wall_clock_s < 5 AND turns absent/0) are
/// skipped — only two INDEPENDENT agent runs producing byte-identical gate
/// output trigger the early stop. Pure for testing.
pub(crate) fn repeated_gate_defect(
    error: &str,
    task_id: &str,
    attempt: u32,
    receipts: &[Receipt],
) -> bool {
    let Some((exit, tail)) = gate_fingerprint(error) else {
        return false;
    };
    if attempt < 2 {
        return false;
    }
    // Prior failed gate receipts for the same task, most-recent first.
    let mut prior: Vec<&Receipt> = receipts
        .iter()
        .filter(|r| {
            r.task == task_id
                && r.attempt < attempt
                && r.outcome == "failed"
                && gate_fingerprint(r.error.as_deref().unwrap_or("")).is_some()
        })
        .collect();
    prior.sort_by_key(|a| std::cmp::Reverse(a.attempt));
    for r in prior {
        // Skip gate-only reruns: near-zero wall clock and no agent turns.
        let is_gate_only_rerun = r.wall_clock_s < 5.0 && r.turns.unwrap_or(0) == 0;
        if is_gate_only_rerun {
            continue;
        }
        // Previous AGENT-driven gate receipt found — compare fingerprints.
        let (prev_exit, prev_tail) = gate_fingerprint(r.error.as_deref().unwrap_or("")).unwrap();
        return prev_exit == exit && prev_tail == tail;
    }
    false
}

/// Reap finished attempts from the channel: worker busy-ness, routing
/// stats, status transitions + persistence. Shared by the main loop and
/// `--once` (single source of truth for the attempt bookkeeping).
///
/// `wait` is the wake-on-completion bound: when `Some(d)` the FIRST
/// receive blocks up to `d` (`recv_timeout`), so the dispatcher is woken
/// the moment an attempt finishes instead of idling through a poll
/// interval — a timeout still returns, so the loop's deadline
/// re-evaluation (deadlock detection, self-heal) keeps its schedule.
/// `None` keeps the drain-only semantics (`--once`'s fast 200ms poll).
///
/// The parameter list is deliberately a flat bundle of the attempt
/// bookkeeping shared verbatim by both call sites (main loop + `--once`);
/// grouping them into a context struct would cost more indirection than
/// the one extra argument saves.
#[allow(clippy::too_many_arguments)] // flat bookkeeping bundle, see above
fn reap(
    rx: &mpsc::Receiver<(String, String, Outcome, f64)>,
    running: &Arc<Mutex<HashMap<String, ()>>>,
    worker_busy: &Arc<Mutex<HashMap<String, usize>>>,
    worker_backoff: &Arc<Mutex<HashMap<String, BackoffState>>>,
    router: &mut Router,
    status: &mut HashMap<String, TaskStatus>,
    store: &Store,
    max_attempts: u32,
    enabled_workers: usize,
    wait: Option<Duration>,
) {
    let first = match wait {
        Some(d) => rx.recv_timeout(d).ok(),
        None => rx.try_recv().ok(),
    };
    for (id, worker_name, outcome, wall_s) in first
        .into_iter()
        .chain(std::iter::from_fn(|| rx.try_recv().ok()))
    {
        running.lock().unwrap().remove(&id);
        let mut wb = worker_busy.lock().unwrap();
        if let Some(c) = wb.get_mut(&worker_name) {
            if *c > 0 {
                *c -= 1;
            }
            if *c == 0 {
                wb.remove(&worker_name);
            }
        }
        drop(wb);
        // Every reaped attempt is a verdict (merged/failed) WITH its
        // measured wall-clock — the same span `execute_task` records on
        // the receipt — so the live duration stat matches a replay from
        // receipts. EXCEPT a failure classified `infra` (provider, timeout,
        // stall, missing binary, git/io, harness): it is not a verdict on
        // the worker, so it must not move the trust stats — exactly as the
        // startup replay excludes it via `counts_as_verdict`. The classifier
        // runs on the same reason string `execute_task` classified, so both
        // paths agree by construction.
        let infra = matches!(&outcome, Outcome::Failed(e) if crate::execute::classify_failure(e) == "infra");
        if !infra {
            router.record(
                &worker_name,
                matches!(outcome, Outcome::Merged | Outcome::Verified),
                Some(wall_s),
            );
            // Any verdict (merged, verified, or non-infra failed) resets
            // the worker's backoff escalation to level 0.
            worker_backoff.lock().unwrap().remove(&worker_name);
        } else {
            // Escalating cooldown: 30 s → 60 s → 120 s → 240 s → 300 s
            // ceiling. A solo worker is capped at 30 s so the campaign
            // keeps trying instead of idling for minutes.
            let mut wb = worker_backoff.lock().unwrap();
            let state = wb.entry(worker_name.clone()).or_insert(BackoffState {
                until: std::time::Instant::now(),
                level: 0,
            });
            state.level += 1;
            let secs = backoff_secs(state.level, enabled_workers);
            state.until = std::time::Instant::now() + std::time::Duration::from_secs(secs);
        }
        // Journal reconciliation: attempt threads persist phase boundaries
        // via load-modify-save (see `record_phase`), so this in-memory map
        // has not seen them. Copy every entry's freshest persisted boundary
        // over before the whole-map save below — otherwise reaping one task
        // would clobber a concurrent attempt's journal (or this attempt's
        // own final boundary) with the stale pre-attempt copy.
        for (k, persisted) in &store.load() {
            if let Some(s) = status.get_mut(k) {
                s.phase = persisted.phase;
            }
        }
        let s = status.entry(id.clone()).or_default();
        s.attempts += 1;
        match outcome {
            Outcome::Merged => {
                s.state = TaskState::Done;
                s.last_error = None;
                println!("  ✓ {id} done (attempt {})", s.attempts);
            }
            Outcome::Verified => {
                s.state = TaskState::Done;
                s.last_error = None;
                println!("  ✓ {id} verified (attempt {})", s.attempts);
            }
            Outcome::Failed(err) => {
                // B: repeated identical gate failure stops early. Two
                // independent agent runs producing byte-identical gate
                // output (same exit code, same failing tail) is a task-
                // design defect, not a flake — stop paying for retries.
                let prior_receipts = store.load_receipts();
                if repeated_gate_defect(&err, &id, s.attempts, &prior_receipts) {
                    let tail = gate_fingerprint(&err).map(|(_, t)| t).unwrap_or_default();
                    s.state = TaskState::Failed;
                    s.last_error = Some(format!(
                        "repeated identical gate failure ({}) — task design defect",
                        tail
                    ));
                    println!(
                        "  ⛐ {id} failed (attempt {}/{}): repeated identical gate failure",
                        s.attempts, max_attempts
                    );
                } else {
                    s.last_error = Some(err.clone());
                    if s.attempts >= max_attempts {
                        s.state = TaskState::Failed;
                        println!(
                            "  ✗ {id} failed (attempt {}/{}): {}",
                            s.attempts, max_attempts, err
                        );
                    } else {
                        s.state = TaskState::Ready;
                        println!("  ✗ {id} attempt {} failed, retrying: {}", s.attempts, err);
                    }
                }
            }
        }
        let _ = store.save(status);
    }
}

/// UCB1 selection among the free, enabled, filter-matching workers.
pub fn pick_worker<'a>(
    cfg: &'a Config,
    busy: &Mutex<HashMap<String, usize>>,
    backoff: &Mutex<HashMap<String, BackoffState>>,
    router: &Router,
    filter: Option<&str>,
) -> Option<&'a crate::config::Worker> {
    let busy = busy.lock().unwrap();
    let backoff = backoff.lock().unwrap();
    let now = std::time::Instant::now();
    let eligible: Vec<&crate::config::Worker> = cfg
        .workers
        .iter()
        .filter(|w| w.enabled)
        .filter(|w| match filter {
            Some(f) => w.name == f,
            None => true,
        })
        .filter(|w| {
            let cap = if w.max_in_flight == 0 {
                1
            } else {
                w.max_in_flight as usize
            };
            busy.get(&w.name).copied().unwrap_or(0) < cap
        })
        .filter(|w| !matches!(backoff.get(&w.name), Some(state) if state.until > now))
        .collect();
    router.pick(eligible)
}

/// Cascade (escalation) routing: the worker for a task's attempt under a
/// declared `cascade`, when one applies. `attempt` is 1-based; the cascade
/// is CHEAPEST-FIRST, so attempt N tries `cascade[N-1]`. Returns `None` when
/// the task has no cascade, the attempt is past the cascade, the named
/// worker is disabled, or it is busy with another task — the caller then
/// falls back to UCB1 exactly as today. The gate remains the sole arbiter:
/// this only changes WHO is dispatched, never whether work is accepted.
pub(crate) fn cascade_worker<'a>(
    cfg: &'a Config,
    task: &crate::config::Task,
    attempt: u32,
    busy: &Mutex<HashMap<String, usize>>,
) -> Option<&'a crate::config::Worker> {
    let cascade = task.cascade.as_ref()?;
    if attempt == 0 {
        return None;
    }
    let idx = (attempt - 1) as usize;
    let name = cascade.get(idx)?;
    let busy = busy.lock().unwrap();
    cfg.workers
        .iter()
        .find(|w| w.name == *name && w.enabled && busy.get(&w.name).copied().unwrap_or(0) == 0)
}

/// The worker for a task's next attempt: the cascade worker when one
/// applies (task has a `cascade` and no explicit `--worker` filter), else
/// the UCB1 pick. An explicit filter overrides the cascade — the operator's
/// pin is a stronger constraint than the task's escalation intent.
pub fn pick_for_attempt<'a>(
    cfg: &'a Config,
    task: &crate::config::Task,
    attempt: u32,
    busy: &Mutex<HashMap<String, usize>>,
    backoff: &Mutex<HashMap<String, BackoffState>>,
    router: &Router,
    filter: Option<&str>,
) -> Option<&'a crate::config::Worker> {
    if filter.is_none() {
        if let Some(w) = cascade_worker(cfg, task, attempt, busy) {
            return Some(w);
        }
    }
    pick_worker(cfg, busy, backoff, router, filter)
}

/// `af run`: parallel dispatch loop for LLM task execution.
///
/// This is the main orchestrator loop. It:
///
/// 1. **Reaps** completed attempts from the channel
/// 2. **Checks budget** against `AF_MAX_WALL_CLOCK_S`
/// 3. **Detects deadlocks** (no ready tasks but work remains)
/// 4. **Dispatches** ready tasks to free workers (UCB1 routing)
/// 5. **Auto-reuses** archived work when scope allows (pre-dispatch)
///
/// The loop uses a **poll-wake** strategy: it polls every `AF_POLL` seconds
/// (default 15s), but wakes immediately when an attempt completes, reducing
/// average dispatch latency to near-zero for busy campaigns.
///
/// **Performance**: Receipt loading is cached and updated incrementally when
/// new attempts complete, avoiding full directory scans on every iteration.
///
/// **Exit codes**:
/// - `0`: all in-scope tasks done
/// - `2`: deadlock detected
/// - `3`: wall-clock budget exhausted
// PidFile: removes the pid file on any exit path of `run_loop` (drop runs on
// normal returns AND on unwinding panics). A `kill -9` leaves it behind;
// `af stop` reports a stale pid file as a warning, never fatal.
#[derive(Default)]
pub(crate) struct PidFile(PathBuf);

impl PidFile {
    pub(crate) fn write(st: &Settings) -> Option<PidFile> {
        let p = st.state_dir.join("af.pid");
        std::fs::write(&p, format!("{}\n", std::process::id())).ok()?;
        Some(PidFile(p))
    }
}

impl Drop for PidFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Parallel dispatch capacity for a round, derived from the CURRENT config.
/// `AF_MAX_PARALLEL` unset/`0` (the default) means "one slot per enabled
/// worker, recomputed on every hot reload": appending a worker to
/// `workers.json` grows the pool mid-run and setting one `enabled: false`
/// shrinks it. An explicit `AF_MAX_PARALLEL=N` is a fixed ceiling that is
/// never recomputed, however the worker set changes. The capacity is always
/// at least `1`, so a malformed mid-run edit can never strand a round.
pub fn pool_capacity(st: &Settings, cfg: &Config) -> usize {
    let cap = if st.max_parallel == 0 {
        cfg.workers.iter().filter(|w| w.enabled).count()
    } else {
        st.max_parallel
    };
    cap.max(1)
}

/// The `(tasks, workers)` mtimes as of the last reload attempt. `None` until
/// the first attempt; files that vanished keep the last good config.
fn cfg_files_mtime(st: &Settings) -> Option<(std::time::SystemTime, std::time::SystemTime)> {
    Some((
        std::fs::metadata(&st.tasks_file).ok()?.modified().ok()?,
        std::fs::metadata(&st.workers_file).ok()?.modified().ok()?,
    ))
}

/// Hot-reload the task/worker config (feedback §6.2). Returns a fresh config
/// only when the files changed since the last ATTEMPT and parse cleanly; a
/// parse/validation failure warns ONCE per generation and yields `None` (the
/// caller keeps the last good config). Mtime-gated so a 15 s poll does not
/// re-parse untouched files.
pub(crate) fn hot_reload(
    st: &Settings,
    seen: &mut Option<(std::time::SystemTime, std::time::SystemTime)>,
) -> Option<crate::config::Config> {
    let mt = cfg_files_mtime(st)?;
    if Some(mt) == *seen {
        return None;
    }
    *seen = Some(mt);
    match crate::config::load_full(
        &st.tasks_file,
        &st.workers_file,
        crate::config::af_var("AF_REPOS_JSON").map(PathBuf::from),
    ) {
        Ok(c) => Some(c),
        Err(e) => {
            eprintln!("warning: config reload failed, keeping last good config: {e}");
            None
        }
    }
}

pub fn run_loop(cfg: &Config, st: &Settings, opts: &RunOptions) -> i32 {
    if opts.dry_run {
        return dry_run(cfg, st);
    }

    // Owned copy: the loop hot-reloads the task/worker config between rounds
    // (feedback §6.2) so an operator's scope fix reaches the next retry
    // without a process restart.
    let mut cfg = cfg.clone();

    let store = Store::new(st.state_dir.clone());
    // Single writer per AF_STATE_DIR: a second `af run` must not clobber
    // run-state.json. Held (and released on drop) for the whole run.
    let _lock = match store.acquire_lock() {
        Ok(g) => g,
        Err(err) => {
            eprintln!("config error: {err}");
            return 2;
        }
    };
    let poll = opts.poll_secs.unwrap_or(st.poll_secs);
    let max_attempts = cfg.defaults.max_attempts;
    let retry_delay_s = cfg.defaults.retry_delay_s;
    // --once dispatches no second attempt, so it never paces one either
    // (the wait would only delay the exit).
    let pace_retries = !opts.once;

    // --- self-heal: remove orphan worktrees from dead attempts ---
    // A corrupt ledger is an ERROR, never a fresh campaign: refusing here is
    // what stops a torn state file from silently re-buying every task.
    let mut status = match store.load_checked() {
        Ok(m) => m,
        Err(err) => {
            eprintln!("config error: {err}");
            return 2;
        }
    };
    let stale_running: Vec<String> = status
        .iter()
        .filter(|(_, s)| s.state == TaskState::Running)
        .map(|(k, _)| k.clone())
        .collect();
    // Multi-repo (ADR-11): heal tries every configured repo + the default.
    let mut repo_list: Vec<(String, PathBuf)> = cfg
        .repos
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    repo_list.push(("(default)".to_string(), st.repo_dir.clone()));
    worktree::heal(
        &repo_list,
        &st.worktree_root,
        &st.branch_prefix,
        &stale_running,
    );
    // Effect-sandwich resume (pi-durable): for each stale `running` task,
    // finish only the effects its dead attempt had not yet committed — a
    // crash must never re-run the expensive, non-replayable agent step.
    for id in &stale_running {
        heal_stale_attempt(&cfg, st, &mut status, id, max_attempts);
    }
    let _ = store.save(&status);

    let log_dir = store.log_dir();
    let _ = std::fs::create_dir_all(&log_dir);
    let _ = std::fs::create_dir_all(store.prompt_dir());

    let (tx, rx) = mpsc::channel::<(String, String, Outcome, f64)>();
    let merge_locks = worktree::MergeLocks::new();
    let worker_busy: Arc<Mutex<HashMap<String, usize>>> = Arc::new(Mutex::new(HashMap::new()));
    let worker_backoff: Arc<Mutex<HashMap<String, BackoffState>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let running: Arc<Mutex<HashMap<String, ()>>> = Arc::new(Mutex::new(HashMap::new()));

    let enabled = cfg.workers.iter().filter(|w| w.enabled).count();
    let mut enabled_workers = enabled;
    let max_parallel = if st.max_parallel == 0 {
        enabled
    } else {
        st.max_parallel
    }
    .max(1);

    let mut ctx = ExecCtx {
        cfg: cfg.clone(),
        st: st.clone(),
        store: store.clone(),
        merge_locks: merge_locks.clone(),
    };

    // Cooperative stop + identity (feedback §6.3/§6.4): the pid file names
    // the orchestrator (af stop signals it); the stop flag file switches the
    // loop into drain mode. Both live in the state dir; the pid file is
    // removed on any exit path (drop guard — a hard kill leaves it behind,
    // and `af stop` reports it as stale, never fatal).
    let _pid = PidFile::write(st);
    let stop_flag = st.state_dir.join("stop");
    let mut draining = false;
    let mut last_cfg_mt = cfg_files_mtime(st);

    println!(
        "agentflow run: {} tasks, {enabled} enabled worker(s), max_parallel={max_parallel}",
        cfg.tasks.len()
    );

    // A --worker filter naming no enabled worker could never dispatch → hang.
    // Fail fast instead.
    if let Some(f) = &opts.worker_filter {
        let ok = cfg.workers.iter().any(|w| w.enabled && &w.name == f);
        if !ok {
            eprintln!("config error: --worker '{f}' matches no enabled worker");
            return 2;
        }
    }

    // In-scope tasks: all tasks, or only the --task filter target. Completion
    // and deadlock are judged on this scope so out-of-scope work never blocks.
    // Rebuilt after every hot-reload (the task set may change mid-run).
    macro_rules! in_scope_of {
        ($cfg:expr) => {
            $cfg.tasks
                .iter()
                .filter(|t| match &opts.task_filter {
                    Some(f) => &t.id == f,
                    None => true,
                })
                .collect::<Vec<&crate::config::Task>>()
        };
    }
    let mut in_scope: Vec<&crate::config::Task> = in_scope_of!(cfg);
    if let Some(f) = &opts.task_filter {
        if in_scope.is_empty() {
            eprintln!("config error: --task '{f}' matches no task");
            return 2;
        }
    }

    // --- dispatch loop ---
    // Pre-dispatch archived-branch reuse (r13-auto-recover): ON by default,
    // switchable off with AF_NO_REUSE=1. `reuse_considered` holds the ARCHIVE
    // BRANCH names already judged this run, so a specific archive is
    // re-validated at most once (a rejected candidate is never retried, and
    // its reason is logged at most once) while the TASK stays reconsiderable
    // for a NEWER archive — including one a failed attempt in this same run
    // just archived. The one task-id burn that remains is the gate-failure
    // fallback: after a SECOND gate failure the next attempt MUST be a fresh
    // agent run, never another re-validation of the same rejected archive.
    let reuse_enabled = !reuse_disabled();
    let mut reuse_considered: HashSet<String> = HashSet::new();
    // Dirty-merge-target pre-flight: which dirty sets were already logged.
    let mut dirty_noted: HashSet<String> = HashSet::new();
    // Measured routing (ADR-12): replay receipts into per-worker stats.
    // Interrupted attempts are NOT worker verdicts (r9-interrupted): a kill
    // that says nothing about the worker must not dent its rate, so filter
    // them out at the call site before they reach the router.
    let all_receipts = store.load_receipts();
    let verdicts: Vec<Receipt> = all_receipts
        .iter()
        .filter(|r| r.counts_as_verdict())
        .cloned()
        .collect();
    let mut router = Router::from_receipts(&verdicts);
    // Performance cache: avoid re-reading receipts on every loop iteration.
    // The receipts only change when a new attempt completes (handled by reap),
    // so we update `all_receipts_cache`, `measured_cache` (wall-clock), and
    // `tokens_cache` (token spend) there.
    let mut all_receipts_cache = all_receipts;
    let mut measured_cache = consumed_wall_clock_s(&all_receipts_cache, &in_scope);
    let mut tokens_cache = consumed_tokens(&all_receipts_cache, &in_scope);
    loop {
        // Reap finished tasks — BLOCKING while attempts are in flight, so
        // a completion wakes the dispatcher immediately instead of it
        // idling through the poll interval. poll_secs stays the UPPER
        // BOUND on how long the loop may idle before re-evaluating
        // (deadlock detection, self-heal and edge cases still re-check on
        // schedule). With nothing in flight no completion can arrive (an
        // attempt is registered in `running` before its thread is
        // spawned), so drain without waiting and re-evaluate at once.
        let wait = if running.lock().unwrap().is_empty() {
            None
        } else {
            Some(Duration::from_secs(poll.max(1)))
        };
        // Reap finished attempts and update receipt cache incrementally.
        // Performance: we reload ALL receipts only when we detect new ones were
        // appended (receipt count changed). typically only 0-1 new receipts
        // per loop iteration, so this avoids full directory scans.
        let receipt_count_before = all_receipts_cache.len();
        reap(
            &rx,
            &running,
            &worker_busy,
            &worker_backoff,
            &mut router,
            &mut status,
            &store,
            max_attempts,
            enabled_workers,
            wait,
        );
        // Check if new receipts were appended (reap or recover writes them).
        // If so, reload and update cache.
        let all_receipts_fresh = store.load_receipts();
        if all_receipts_fresh.len() != receipt_count_before {
            all_receipts_cache = all_receipts_fresh;
            measured_cache = consumed_wall_clock_s(&all_receipts_cache, &in_scope);
            tokens_cache = consumed_tokens(&all_receipts_cache, &in_scope);
        }

        // Hot-reload (feedback §6.2): plan this round against the CURRENT
        // tasks/workers files. A reload failure keeps the last good config
        // (warn once per broken generation) — a typo must not kill a campaign.
        if let Some(fresh) = hot_reload(st, &mut last_cfg_mt) {
            enabled_workers = fresh.workers.iter().filter(|w| w.enabled).count();
            println!(
                "config reloaded: {} task(s), {} worker(s), pool capacity {}",
                fresh.tasks.len(),
                fresh.workers.len(),
                pool_capacity(st, &fresh)
            );
            cfg = fresh;
            ctx.cfg = cfg.clone();
            in_scope = in_scope_of!(cfg);
        }

        // Cooperative stop (feedback §6.4): no NEW dispatches while draining;
        // in-flight attempts finish through their normal reap path.
        if !draining && stop_flag.exists() {
            let _ = std::fs::remove_file(&stop_flag);
            draining = true;
            println!("-- stop requested: draining in-flight attempts (no new dispatches) --");
        }
        if draining && running.lock().unwrap().is_empty() {
            println!("\nStopped (drained; in-flight attempts finished).");
            println!("{}", board_of(&cfg, &status));
            return 3;
        }

        // Terminal conditions (judged on the in-scope tasks only).
        let all_done = in_scope
            .iter()
            .all(|t| matches!(status.get(&t.id).map(|s| &s.state), Some(TaskState::Done)));
        if all_done {
            println!("\nAll tasks done.");
            println!("{}", board_of(&cfg, &status));
            return 0;
        }
        // Cost ceilings (r8-budget-cap + token-budget-ceiling): use cached
        // receipts for performance. Updated incrementally by reap() when new
        // receipts are appended. 0 disables either ceiling entirely.
        let measured = if st.max_wall_clock_s == 0 {
            0.0
        } else {
            measured_cache
        };
        let measured_tokens = if st.max_tokens == 0 { 0 } else { tokens_cache };
        let over_budget = wall_clock_budget_exhausted(st, measured)
            || token_budget_exhausted(st, measured_tokens);

        let running_ids: Vec<String> = running.lock().unwrap().keys().cloned().collect();
        if running_ids.is_empty() {
            // Nothing in flight: progress is possible only if something is
            // ready. Otherwise this is a deadlock (failed/absent deps, or a
            // --task target waiting on out-of-scope work).
            let ready_in_scope = scheduler::ready_tasks(&cfg, &status, &[], max_attempts)
                .into_iter()
                .filter(|t| in_scope.iter().any(|s| s.id == t.id))
                .count();
            // Budget stop TERMINATES here: a ready-but-undispatched task
            // would otherwise keep the "progress is possible" check happy
            // forever, spinning the loop. Completion above already won, so
            // reaching this point means work genuinely remains.
            if over_budget && ready_in_scope > 0 {
                let not_started = in_scope
                    .iter()
                    .filter(|t| {
                        matches!(
                            status.get(&t.id).map(|s| &s.state),
                            None | Some(TaskState::Ready)
                        )
                    })
                    .count();
                if token_budget_exhausted(st, measured_tokens) {
                    eprintln!(
                        "token budget exhausted: spent {measured_tokens} of {} tokens cap; {not_started} in-scope task(s) not started",
                        st.max_tokens
                    );
                } else {
                    eprintln!(
                        "wall-clock budget exhausted: spent {measured:.1}s of {}s cap; {not_started} in-scope task(s) not started",
                        st.max_wall_clock_s
                    );
                }
                return 3;
            }
            if ready_in_scope == 0 {
                let blocked = scheduler::find_deadlock_in(&cfg, &status, &[], &in_scope)
                    .unwrap_or_else(|| in_scope.iter().map(|t| t.id.clone()).collect());
                eprintln!(
                    "DEADLOCK: no task can make progress. Blocked: {}",
                    blocked.join(", ")
                );
                eprintln!("{}", board_of(&cfg, &status));
                return 2;
            }
        }

        // Dispatch a round — never while the ceiling is exhausted: attempts
        // already in flight are allowed to finish, only NEW ones are blocked.
        // The ceiling is RE-derivable every round: when the operator left
        // `AF_MAX_PARALLEL` to the default, the pool follows the current
        // enabled-worker count (pool_capacity over the reloaded config), so
        // adding/disabling workers mid-run grows/shrinks the pool live.
        let max_parallel = pool_capacity(st, &cfg);
        if !over_budget && !draining && running.lock().unwrap().len() < max_parallel {
            let running_now = running.lock().unwrap().keys().cloned().collect::<Vec<_>>();
            let ready = scheduler::ready_tasks(&cfg, &status, &running_now, max_attempts);
            // True when this pass parked ≥1 task for a dirty merge target.
            // With every ready task parked, nothing is in flight on the next
            // iteration, so the reap has nothing to block on and the pass
            // would hot-spin re-checking git status ([spin], fixed below).
            let mut parked_pass = false;
            for t in ready {
                if let Some(f) = &opts.task_filter {
                    if &t.id != f {
                        continue;
                    }
                }
                if matches!(status.get(&t.id).map(|s| &s.state), Some(TaskState::Done)) {
                    continue;
                }
                if running.lock().unwrap().len() >= max_parallel {
                    break;
                }
                // PRE-DISPATCH REUSE: a rejected attempt's work is already on
                // disk under an archived branch, so re-buying it from a fresh
                // agent is pure waste. Checked before a worker is picked
                // (reuse consumes no worker slot); the gate still decides.
                if reuse_enabled
                    && reuse_before_dispatch(
                        &cfg,
                        st,
                        &merge_locks,
                        &t,
                        &mut status,
                        &mut reuse_considered,
                    )
                {
                    continue;
                }
                // DIRTY-MERGE-TARGET PRE-FLIGHT (2026-11-24): a
                // gate-passing attempt could not be merged into a dirty
                // checkout, so paying a 30-minute agent run for it is pure
                // waste. Park the dispatch — no receipt, no attempt spent;
                // the loop re-checks every round and resumes by itself once
                // the operator commits/stashes. Logged once per dirty set.
                let target = cfg.repo_dir_for(&t, &st.repo_dir);
                // An UNREADABLE status (not a repo, missing dir) is NOT a
                // park: that is the attempt's own error to report (the
                // worktree arm fails it cleanly). Parking here would spin
                // the loop forever on a task that can never dispatch.
                if let Ok(Some(p)) = worktree::first_dirty_path(&target) {
                    let what = format!("{} ({p})", target.display());
                    // First sighting this run: log it AND persist a status
                    // entry so `af status` explains the stall (the receipt
                    // stays unwritten — no attempt was spent).
                    if dirty_noted.insert(what.clone()) {
                        eprintln!(
                            "  ⏸ merge target dirty: {what}; dispatch parked until it is clean"
                        );
                        let s = status.entry(t.id.clone()).or_default();
                        s.state = TaskState::Ready;
                        s.last_error = Some(format!("waiting: merge target dirty ({what})"));
                        let _ = store.save(&status);
                    }
                    parked_pass = true;
                    continue;
                }
                // Per-task token ceiling: a task whose receipts already total
                // at least its `max_tokens` is not dispatched — it is set to
                // `failed` with a reason, exactly as `max_attempts` exhausts
                // a task. 0 or absent = no per-task cap. This is a dispatch
                // gate, not a mid-attempt kill (the provider reports usage
                // after the round-trip). Composes with the campaign ceiling.
                if let Some(cap) = t.max_tokens {
                    if cap > 0 {
                        let spent = consumed_tokens_for_task(&all_receipts_cache, &t.id);
                        if spent >= cap {
                            let s = status.entry(t.id.clone()).or_default();
                            s.state = TaskState::Failed;
                            s.last_error = Some(format!(
                                "token budget exhausted: spent {spent} of {cap} max_tokens"
                            ));
                            let _ = store.save(&status);
                            eprintln!(
                                "  \u{2298} {} token budget exhausted: {spent}/{cap} tokens",
                                t.id
                            );
                            continue;
                        }
                    }
                }
                // Cascade (escalation) routing: a task with a `cascade` runs
                // the CHEAPEST worker first (attempt 1 = cascade[0]) and
                // escalates to the next on each retry, so a hard task pays
                // only the escalation it needs. The gate stays the sole
                // arbiter — a cascade only changes WHO is dispatched.
                // `pick_for_attempt` consults the cascade (skipped when an
                // explicit `--worker` filter is set) and falls back to UCB1
                // when the attempt is past the cascade, the cascade worker
                // is disabled, or it is busy. The attempt number is read
                // from persisted status (next attempt = attempts + 1)
                // BEFORE worker selection.
                let next_attempt = status.get(&t.id).map(|s| s.attempts).unwrap_or(0) + 1;
                let worker = match pick_for_attempt(
                    &cfg,
                    &t,
                    next_attempt,
                    &worker_busy,
                    &worker_backoff,
                    &router,
                    opts.worker_filter.as_deref(),
                ) {
                    Some(w) => w.clone(),
                    None => break, // no free worker this round
                };
                // Mark Running + persist BEFORE spawning (crash-safety).
                let attempt = {
                    let s = status.entry(t.id.clone()).or_default();
                    s.state = TaskState::Running;
                    // Fresh attempt: the journal restarts at Spawned (the
                    // worker commits it before spawning the agent). A crash
                    // in that window resumes as RerunAgent — always safe.
                    s.phase = None;
                    // Attempt identity (r11-attempt-identity): same save,
                    // same window. A crash after this point still leaves
                    // behind WHO ran the attempt and WHEN it started, so
                    // the startup heal can name the worker and measure an
                    // upper-bound duration instead of a placeholder.
                    s.attempt_started_ts = Some(crate::state::now_ts());
                    s.attempt_worker = Some(worker.name.clone());
                    s.attempts + 1
                };
                let _ = store.save(&status);

                *worker_busy
                    .lock()
                    .unwrap()
                    .entry(worker.name.clone())
                    .or_insert(0) += 1;
                running.lock().unwrap().insert(t.id.clone(), ());
                let log_path = log_dir.join(format!("{}.log", t.id));
                let tx2 = tx.clone();
                let ctx2 = ctx.clone();
                let id = t.id.clone();
                let wname = worker.name.clone();
                let wclone = worker.clone();
                let model = &worker.model;
                println!(
                    "  → {id} dispatch on {} ({model}) [attempt {attempt}]",
                    worker.name
                );
                std::thread::spawn(move || {
                    // The attempt's wall-clock, measured around the same
                    // span `execute_task` records on its receipt
                    // (`wall_clock_s`), so the router's live duration stat
                    // is the same statistic a replay from receipts would
                    // build. The retry-pacing wait below is deliberately
                    // OUTSIDE the measurement — it is post-attempt backoff,
                    // not work.
                    let t0 = std::time::Instant::now();
                    let out = execute::execute_task(&ctx2, &wclone, &id, attempt, &log_path);
                    let wall_s = t0.elapsed().as_secs_f64();
                    // Retry pacing (`retry_delay_s`, default 0): after a
                    // FAILED attempt that will be retried — `attempt <
                    // max_attempts`; reap will count this attempt and send
                    // the task back to Ready — wait the configured backoff
                    // before reporting the outcome, so the next attempt
                    // cannot start until it has elapsed. The wait lives in
                    // THIS attempt's thread, on the retry path ONLY:
                    //   * a first attempt is never delayed (it starts on
                    //     dispatch, and a task that merges first try never
                    //     reaches this branch), and
                    //   * the dispatcher loop never sleeps for it — round 8
                    //     removed the global poll sleep, and a global wait
                    //     would stall every worker slot and re-introduce
                    //     that idle time. With several workers in flight
                    //     the wait belongs to the retrying task, so the
                    //     other slots keep dispatching.
                    if pace_retries
                        && retry_delay_s > 0
                        && attempt < max_attempts
                        && matches!(out, Outcome::Failed(_))
                    {
                        // Observable: one clear line naming the task and
                        // the seconds, in the run log and the task's own
                        // log, so a puzzling pause is explainable.
                        println!(
                            "  \u{23f8} {id} attempt {attempt} failed — pacing retry: waiting {retry_delay_s}s (retry_delay_s)"
                        );
                        if let Ok(mut f) = std::fs::OpenOptions::new().append(true).open(&log_path)
                        {
                            let _ = writeln!(
                                f,
                                "af: pacing retry — waiting {retry_delay_s}s (retry_delay_s) before attempt {}",
                                attempt + 1
                            );
                        }
                        std::thread::sleep(Duration::from_secs(retry_delay_s));
                    }
                    let _ = tx2.send((id, wname, out, wall_s));
                });
            }
            if opts.once {
                // --once: dispatch no more after this round; wait for in-flight.
                while !running.lock().unwrap().is_empty() {
                    reap(
                        &rx,
                        &running,
                        &worker_busy,
                        &worker_backoff,
                        &mut router,
                        &mut status,
                        &store,
                        max_attempts,
                        enabled_workers,
                        None,
                    );
                    std::thread::sleep(Duration::from_millis(200));
                }
                println!("{}", board_of(&cfg, &status));
                return 0;
            }
            // PACE-when-parked: with nothing in flight the reap blocks on
            // nothing, and a ready-but-dirty task set would otherwise spin
            // this loop at full speed re-checking git status. poll_secs is
            // the idle bound. A clean checkout picks the fix up on the very
            // next wake (re-check is live, `parked_pass` is per-pass).
            if parked_pass && running.lock().unwrap().is_empty() {
                std::thread::sleep(Duration::from_secs(poll.max(1)));
            }
        }

        // No trailing sleep: the next iteration's reap blocks on the
        // completion channel while work is in flight (waking the instant
        // an attempt finishes) and bounds its wait by poll_secs otherwise.
    }
}
