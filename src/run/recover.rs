//! Resume / recovery machinery: effect-sandwich healing of stale
//! `running` attempts, idempotent merge tails, and `af recover`.

use super::reuse::append_recovered_receipt;
use super::UNKNOWN_WORKER;
use crate::config::{Config, Settings, TaskState};
use crate::execute;
use crate::gate;
use crate::state::{
    resume_action, AttemptPhase, Receipt, ResumeAction, Store, TaskStatus, OUTCOME_INTERRUPTED,
};
use crate::subprocess::{self, EnvMode};
use crate::worktree;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// git helper for the resume paths (same trusted-child policy as
/// worktree.rs's own git operations: full inherited env, hard timeout).
fn git_run(repo: &Path, args: &[&str]) -> subprocess::CmdOut {
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    subprocess::run(
        "git",
        &args,
        Some(repo),
        &[],
        EnvMode::Inherit,
        Duration::from_secs(300),
    )
}

/// Does the attempt branch still exist in the repo? (The agent's durable
/// work lives there.)
pub(crate) fn branch_exists(repo: &Path, branch: &str) -> bool {
    git_run(
        repo,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    )
    .passed()
}

/// Is `branch` already fully contained in the repo's checked-out base?
/// `pub(crate)` so `execute_attempt`'s belt-and-braces merge verification
/// ("only report merged when the base really contains the work") reuses the
/// exact same ancestor check as the resume machinery.
pub(crate) fn branch_merged_into_head(repo: &Path, branch: &str) -> bool {
    git_run(repo, &["merge-base", "--is-ancestor", branch, "HEAD"]).passed()
}

/// Is this task's af merge commit already in the base's history? Covers the
/// crash window AFTER a successful merge whose cleanup already deleted the
/// branch — the task's work is in the base, so "record merged" is exact.
pub(crate) fn merge_commit_in_base(repo: &Path, id: &str) -> bool {
    let out = git_run(
        repo,
        &[
            "log",
            "--format=%H",
            "-1",
            "--grep",
            &format!("^af: {id} — "),
            "HEAD",
        ],
    );
    out.passed() && !out.stdout.trim().is_empty()
}

/// Keep/repair the dead attempt's worktree so the gate can re-run inside
/// it: a valid worktree is reused as-is; anything else (missing dir, or a
/// raw leftover directory without git metadata) is rebuilt from the
/// EXISTING branch (`git worktree add <path> <branch>` — no new branch, so
/// the committed agent work is never discarded). None when git cannot
/// materialize it → caller falls back to RerunAgent.
fn ensure_worktree_on_branch(
    repo: &Path,
    wt_root: &Path,
    id: &str,
    branch: &str,
) -> Option<PathBuf> {
    let wt_path = wt_root.join(id);
    // A live worktree carries a `.git` file/dir; a stale plain directory
    // does not and must be cleared before `worktree add`.
    if wt_path.join(".git").exists() {
        return Some(wt_path);
    }
    let _ = std::fs::remove_dir_all(&wt_path);
    std::fs::create_dir_all(wt_root).ok()?;
    // Drop stale registrations left by raw-deleted worktree dirs (same
    // reason as worktree::create) so the branch is free to check out.
    let _ = git_run(repo, &["worktree", "prune"]);
    let out = git_run(repo, &["worktree", "add", wt_path.to_str()?, branch]);
    if out.passed() {
        Some(wt_path)
    } else {
        None
    }
}

/// Reset a resumed-and-failed attempt exactly like a normally failed one:
/// terminal Failed at max attempts, else Ready for a fresh agent attempt.
/// The budget check sees a fully-consumed count: `heal_stale_attempt` has
/// already counted the interrupted attempt before dispatching the resume.
pub(crate) fn mark_resumed_failure(
    status: &mut HashMap<String, TaskStatus>,
    id: &str,
    max_attempts: u32,
    err: &str,
) {
    if let Some(s) = status.get_mut(id) {
        s.last_error = Some(err.to_string());
        s.phase = None; // next attempt journals from Spawned again
        s.state = if s.attempts >= max_attempts {
            TaskState::Failed
        } else {
            TaskState::Ready
        };
    }
}

/// Verify-task tail of the resume: the gate already passed (or the agent
/// stopped with no committed change), so the attempt branch is NEVER merged —
/// it is archived under `<branch>-rejected-<now>` so the durable work survives
/// for recovery while the base branch stays clean, then the attempt branch +
/// worktree are dropped. The task is marked Done and a `verified` receipt is
/// recorded, mirroring `execute_attempt`'s verify branch. Returns true.
fn finish_resume_verify(
    st: &Settings,
    status: &mut HashMap<String, TaskStatus>,
    id: &str,
    repo: &Path,
    branch: &str,
) -> bool {
    // Archive a COPY of the attempt branch tip when it carries real work
    // beyond the base (same commit-ahead guard `preserve_work` uses), so the
    // paid-for agent work survives the cleanup that follows. `None` attempt
    // keeps the legacy `<branch>-rejected-<now>` name, exactly like the
    // startup-heal archiving in `worktree::clean`.
    let kept = match worktree::current_branch(repo).ok() {
        Some(base) if matches!(worktree::commits_ahead(repo, &base, branch), Ok(n) if n > 0) => {
            worktree::archive_branch(repo, branch, None, crate::state::now_ts())
        }
        _ => None,
    };
    if let Some(name) = &kept {
        println!("  ✓ {id} resumed to done (verify archived on {name}, agent not re-run)");
    } else {
        println!("  ✓ {id} resumed to done (verify archived nothing, agent not re-run)");
    }
    if let Some(s) = status.get_mut(id) {
        s.state = TaskState::Done;
        s.last_error = None;
        s.phase = Some(AttemptPhase::GatePassed);
    }
    // Drop the attempt branch + worktree (best-effort, exactly like the merge
    // resume's cleanup). The archived COPY survives this.
    worktree::remove(
        repo,
        &worktree::Worktree {
            path: st.worktree_root.join(id),
            branch: branch.to_string(),
        },
    );
    // Record the verified outcome for the resumed (counted) attempt.
    let store = Store::new(st.state_dir.clone());
    let attempt = status.get(id).map(|s| s.attempts).unwrap_or(0);
    let _ = store.append_receipt(&Receipt {
        task: id.to_string(),
        attempt,
        worker: UNKNOWN_WORKER.to_string(),
        model: UNKNOWN_WORKER.to_string(),
        wall_clock_s: 0.0,
        tokens: None,
        turns: None,
        prompt_tokens: None,
        completion_tokens: None,
        ts: crate::state::now_ts(),
        outcome: "verified".to_string(),
        error: None,
        cost_micros: None,
        contracts: None,
        failure_class: None,
    });
    true
}

/// Shared tail of both resume paths: land the attempt branch in the base
/// IDEMPOTENTLY (merge — or just record merged when the branch is already
/// an ancestor / the task's merge commit is already in the base), mark the
/// task Done, and drop the attempt's worktree + branch. Returns false only
/// when the task is unknown (caller falls back to RerunAgent).
fn finish_resume_merge(
    cfg: &Config,
    st: &Settings,
    status: &mut HashMap<String, TaskStatus>,
    id: &str,
    max_attempts: u32,
    repo: &Path,
    branch: &str,
) -> bool {
    let Some(task) = cfg.by_id.get(id) else {
        return false;
    };
    // A verify task NEVER merges: the crash-recovery resume archives the
    // branch and records `verified` instead, exactly like the live execute
    // path's verify branch.
    if task.verify {
        return finish_resume_verify(st, status, id, repo, branch);
    }
    let already_merged = branch_merged_into_head(repo, branch) || merge_commit_in_base(repo, id);
    let merged = if already_merged {
        Ok(()) // idempotent: record merged without a second merge
    } else {
        worktree::merge(
            repo,
            branch,
            &worktree::MergeLocks::new(),
            &format!("af: {} — {}", task.id, task.title),
        )
    };
    match merged {
        Ok(()) => {
            if let Some(s) = status.get_mut(id) {
                s.state = TaskState::Done;
                s.last_error = None;
                s.phase = Some(AttemptPhase::GatePassed);
            }
            // Merge consumed the attempt — drop its worktree + branch
            // (best-effort, exactly like execute_attempt's cleanup).
            worktree::remove(
                repo,
                &worktree::Worktree {
                    path: st.worktree_root.join(id),
                    branch: branch.to_string(),
                },
            );
            println!("  ✓ {id} resumed to done (agent not re-run)");
            true
        }
        Err(e) => {
            mark_resumed_failure(
                status,
                id,
                max_attempts,
                &format!("resume merge failed: {e}"),
            );
            true
        }
    }
}

/// RerunGate resume (phase = AgentDone): the agent already committed its
/// change on `<prefix>/<id>` — re-run ONLY the acceptance gate in a
/// kept/repaired worktree, then merge on success. Never re-invokes the
/// agent. Returns false when the resume artifacts are gone (branch/worktree
/// cannot be recovered) → caller falls back to RerunAgent (always safe).
fn resume_gate_only(
    cfg: &Config,
    st: &Settings,
    status: &mut HashMap<String, TaskStatus>,
    id: &str,
    max_attempts: u32,
) -> bool {
    let Some(task) = cfg.by_id.get(id) else {
        return false;
    };
    let repo = cfg.repo_dir_for(task, &st.repo_dir);
    let branch = format!("{}/{}", st.branch_prefix, id);
    if !branch_exists(&repo, &branch) {
        return false; // the agent's durable work is gone — only a re-run recovers
    }
    let Some(wt_path) = ensure_worktree_on_branch(&repo, &st.worktree_root, id, &branch) else {
        return false;
    };
    // Re-run only the gate. Manual/gateless attempts have nothing to check —
    // the agent step was their last pre-merge effect.
    let gate_ok = if !task.manual {
        match &task.accept {
            Some(accept) => gate::run_accept(
                accept,
                &wt_path,
                &st.gate_env,
                Duration::from_secs(cfg.defaults.accept_timeout_s),
                task.gate_replay,
            )
            .passed(),
            None => true,
        }
    } else {
        true
    };
    if !gate_ok {
        mark_resumed_failure(status, id, max_attempts, "resumed acceptance gate failed");
        return true;
    }
    // Journal the boundary in the in-memory map (persisted by the heal's
    // single save): gate passed — only the merge remains.
    if let Some(s) = status.get_mut(id) {
        s.phase = Some(AttemptPhase::GatePassed);
    }
    finish_resume_merge(cfg, st, status, id, max_attempts, &repo, &branch)
}

/// MergeOnly resume (phase = GatePassed): gate already passed — land the
/// branch idempotently (or record merged when it already is). Never invokes
/// agent or gate. Falls back to RerunAgent only when there is NO evidence of
/// the work: no branch AND no merge commit in the base.
fn resume_merge_only(
    cfg: &Config,
    st: &Settings,
    status: &mut HashMap<String, TaskStatus>,
    id: &str,
    max_attempts: u32,
) -> bool {
    let Some(task) = cfg.by_id.get(id) else {
        return false;
    };
    let repo = cfg.repo_dir_for(task, &st.repo_dir);
    let branch = format!("{}/{}", st.branch_prefix, id);
    if !branch_exists(&repo, &branch) && !merge_commit_in_base(&repo, id) {
        return false; // → RerunAgent
    }
    finish_resume_merge(cfg, st, status, id, max_attempts, &repo, &branch)
}

/// Startup heal for one stale `running` task — the effect-sandwich resume.
/// RerunAgent: fresh attempt from Ready (the stale worktree is replaced
/// when the retry is dispatched). RerunGate / MergeOnly: finish only the
/// remaining effects, never re-invoking the agent.
///
/// The interrupted attempt consumes the retry budget. `attempts` counts
/// STARTED attempts, and exactly two writers keep that true: `reap`
/// increments when an attempt reports its outcome, and this heal increments
/// for an attempt that was started (dispatch persisted `running` with
/// attempt number `attempts + 1`) but died before `reap` could run. Each
/// started attempt is counted exactly once because a reaped attempt is no
/// longer `running` — only one of the two writers can ever fire for it.
/// Without this, healing rewound the task to Ready for free, so a crash
/// loop (OOM kill, reboot, kill -9) could re-buy the agent forever without
/// ever reaching `max_attempts`. Receipt numbering stays coherent: the
/// dead attempt's receipts (if any) carry the number it was dispatched
/// with, and the next dispatch computes `attempts + 1` on the incremented
/// count — a strictly greater number — so `af cost` never double-counts
/// one started attempt under two numbers.
pub(crate) fn heal_stale_attempt(
    cfg: &Config,
    st: &Settings,
    status: &mut HashMap<String, TaskStatus>,
    id: &str,
    max_attempts: u32,
) {
    // Count the interrupted attempt BEFORE any resume decision, so every
    // resume outcome — the reset below, and `mark_resumed_failure`'s
    // budget check inside the gate/merge resumes — sees the budget as
    // already consumed by the interrupted attempt.
    if let Some(s) = status.get_mut(id) {
        s.attempts += 1;
    }
    let action = resume_action(status.get(id).and_then(|s| s.phase));
    let handled = match action {
        ResumeAction::RerunGate => resume_gate_only(cfg, st, status, id, max_attempts),
        ResumeAction::MergeOnly => resume_merge_only(cfg, st, status, id, max_attempts),
        ResumeAction::RerunAgent => false, // handled by the reset below
    };
    if !handled {
        // Record the lost attempt (r9-interrupted): heal could not resume
        // its durable agent outcome, so it produced no verdict and the
        // process died. Attempt identity (r11-attempt-identity): dispatch
        // persisted WHO ran the attempt and WHEN it started, so the
        // receipt tells the truth when they are known — the persisted
        // worker and its configured model, plus a duration measured from
        // dispatch. That duration is an UPPER BOUND, not a measurement of
        // the attempt: the exit time is unknowable, only the start is
        // durable. The receipt keeps the attempt visible in `af cost`
        // (interrupted, not a worker loss). An older state file (or a
        // crash before the first dispatch save) has neither field and
        // keeps the exact pre-change placeholder: `unknown`/0.0s.
        // The attempt number is the one dispatch persisted
        // (`attempts + 1`), exactly the incremented count above.
        // NOTE: a successful gate/merge resume is NOT lost — its agent work
        // is durable and finishes as merged — so it records no receipt here.
        let store = Store::new(st.state_dir.clone());
        let attempt = status.get(id).map(|s| s.attempts).unwrap_or(0);
        let identity = status
            .get(id)
            .map(|s| (s.attempt_started_ts, s.attempt_worker.clone()));
        let (started_ts, worker_name) = identity.unwrap_or((None, None));
        // The worker: the one dispatch chose, when that choice survived.
        let worker = worker_name
            .clone()
            .unwrap_or_else(|| UNKNOWN_WORKER.to_string());
        // The model: ONLY the configured worker's — a healed attempt must
        // never claim a model nobody recorded (a worker since removed from
        // the config keeps the placeholder for the model).
        let model = worker_name
            .as_deref()
            .and_then(|name| cfg.workers.iter().find(|w| w.name == name))
            .map(|w| w.model.clone())
            .unwrap_or_else(|| UNKNOWN_WORKER.to_string());
        let (wall_clock_s, error) = match started_ts {
            Some(ts) => (
                crate::state::now_ts().saturating_sub(ts) as f64,
                "orchestrator exited mid-attempt; duration is time since dispatch (the exit time is unknown, so this is an upper bound)"
                    .to_string(),
            ),
            None => (
                0.0,
                "orchestrator exited mid-attempt; attempt duration unknown".to_string(),
            ),
        };
        let _ = store.append_receipt(&Receipt {
            task: id.to_string(),
            attempt,
            worker,
            model,
            wall_clock_s,
            tokens: None,
            turns: None,
            prompt_tokens: None,
            completion_tokens: None,
            ts: crate::state::now_ts(),
            outcome: OUTCOME_INTERRUPTED.to_string(),
            error: Some(error),
            cost_micros: None,
            contracts: None,
            failure_class: None,
        });
        // RerunAgent, or a resume that lost its artifacts (always safe):
        // apply the budget to the now-counted attempt — Ready for a fresh
        // agent attempt while budget remains (the stale worktree is
        // dropped by worktree::create when the retry is dispatched), or
        // terminal Failed once the crash loop has started `max_attempts`
        // attempts, so it can never re-buy the agent again.
        if let Some(s) = status.get_mut(id) {
            if s.attempts >= max_attempts {
                s.state = TaskState::Failed;
                s.last_error = Some(format!(
                    "previous run interrupted; retry budget exhausted ({}/{max_attempts})",
                    s.attempts
                ));
            } else {
                s.state = TaskState::Ready;
                s.last_error = Some("previous run interrupted".into());
            }
            s.phase = None; // next attempt journals from Spawned again
        }
    }
}

/// `af recover --task ID [--attempt N] [--dry-run]`: re-validate an archived
/// rejected branch — the copy round 11's failure paths keep under
/// `<prefix>/<id>-rejected-<attempt>-<ts>[-<n>]` (or the legacy
/// `<prefix>/<id>-rejected-<ts>[-<n>]`) — against the task's CURRENT scope
/// and gate, and merge it into the base when it passes. The agent is never
/// re-invoked: the operator already paid for this work once; recovery just
/// un-blocks it when the reason it was rejected is gone.
///
/// Selection picks the NEWEST archived branch for `id` by parsing the
/// numeric `<ts>` (then the `-<n>` suffix), never by git order or dates.
/// With `--attempt N` it instead restricts selection to archives whose name
/// parses to exactly attempt `N` (legacy attempt-less archives parse as `0`),
/// so the operator can recover a specific paid-for attempt rather than the
/// latest one.
///
/// On success the failed attempt's receipt is left untouched (history is
/// append-only) and a NON-VERDICT `recovered` receipt is appended, naming the
/// archived attempt and its failed receipt's worker/model so `af cost` can
/// pair the reclaim out of WASTED.
///
/// Exit codes: 0 merged / dry-run reported; 1 re-validation failed (scope
/// violation or gate failure), with the archived branch AND its worktree
/// handled so no failure leaves a worktree behind (the archived branch itself
/// survives a rejection); 2 unknown task, a requested attempt with no archive,
/// or no archived branch to recover (nothing to recover is not a failure).
pub fn recover(cfg: &Config, st: &Settings, id: &str, attempt: Option<u32>, dry_run: bool) -> i32 {
    let Some(task) = cfg.by_id.get(id).cloned() else {
        eprintln!("config error: unknown task '{id}'");
        return 2;
    };
    let repo = cfg.repo_dir_for(&task, &st.repo_dir);
    let branches = worktree::archived_branches(&repo, &st.branch_prefix);
    let selected = match attempt {
        // `--attempt N`: only archives that parse to exactly attempt N. A
        // legacy attempt-less archive parses as 0, so it needs `--attempt 0`.
        Some(n) => worktree::archived_branch_for_attempt(&branches, &st.branch_prefix, id, n),
        // No flag: the newest archive for the task, whatever attempt it names.
        None => worktree::newest_archived_branch(&branches, &st.branch_prefix, id),
    };
    let Some(branch) = selected else {
        match attempt {
            Some(n) => eprintln!(
                "no archived branch for task '{id}' at attempt {n} (searched <prefix>/<id>-rejected-{n}-<ts>)"
            ),
            None => eprintln!(
                "no archived branch for task '{id}' (searched <prefix>/<id>-rejected-<ts>)"
            ),
        }
        return 2;
    };

    // --dry-run: selection only — report exactly what recovery WOULD do and
    // touch nothing (no worktree, no gate run, no merge).
    if dry_run {
        println!("would recover {branch} to done (agent not re-run)");
        return 0;
    }

    // Re-validation, in order, before any merge. The archived branch is
    // checked out in a throwaway worktree so both the scope diff and the
    // acceptance gate run against its committed state, exactly as an
    // attempt path would.
    let base_branch = match worktree::current_branch(&repo) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    let wt = match worktree::attach_existing(&repo, &st.worktree_root, id, &branch) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("error: cannot check out {branch}: {e}");
            return 1;
        }
    };
    // (a) SCOPE: every path the archived branch changes relative to the base
    // must be covered by the task's CURRENT scope (the whole point: the
    // operator widened it). An empty scope allows any file.
    let changed = match execute::changed_paths(&wt.path, &base_branch) {
        Ok(c) => c,
        Err(e) => {
            worktree::remove_worktree_only(&repo, &wt);
            eprintln!("error: {e}");
            return 1;
        }
    };
    let violations = execute::scope_violations(&changed, &task.scope);
    if !violations.is_empty() {
        eprintln!(
            "recover {id}: attempt edited files out of scope: {} (allowed: {})",
            violations.join(", "),
            task.scope.join(", ")
        );
        worktree::remove_worktree_only(&repo, &wt);
        return 1;
    }
    // (b) GATE: unless the task is manual, re-run the acceptance gate exactly
    // as the attempt path does (including the task's `gate_replay`). A
    // non-zero gate rejects the recovery.
    if !task.manual {
        if let Some(accept) = &task.accept {
            let gate_out = gate::run_accept(
                accept,
                &wt.path,
                &st.gate_env,
                Duration::from_secs(cfg.defaults.accept_timeout_s),
                task.gate_replay,
            );
            if !gate_out.passed() {
                eprintln!(
                    "acceptance gate failed (exit {}): {}",
                    gate_out
                        .code
                        .map(|c| c.to_string())
                        .unwrap_or_else(|| "-".into()),
                    gate_out.combined().trim()
                );
                worktree::remove_worktree_only(&repo, &wt);
                return 1;
            }
        }
    }
    // Success path: merge the archived branch into the base with the standard
    // merge message, mark the task Done (phase = GatePassed), persist it, and
    // remove the recovered worktree + the archived branch (its content is now
    // in the base — leaving it behind would be a ghost for `af clean`).
    let msg = format!("af: {} — {}", task.id, task.title);
    if let Err(e) = worktree::merge(&repo, &branch, &worktree::MergeLocks::new(), &msg) {
        worktree::remove_worktree_only(&repo, &wt);
        eprintln!("error: {e}");
        return 1;
    }
    let store = Store::new(st.state_dir.clone());
    let mut status = store.load();
    let s = status.entry(id.to_string()).or_default();
    s.state = TaskState::Done;
    s.last_error = None;
    s.phase = Some(AttemptPhase::GatePassed);
    let _ = store.save(&status);
    append_recovered_receipt(&store, st, id, &branch);
    worktree::remove(&repo, &wt);
    println!("✓ {id} recovered to done (agent not re-run)");
    0
}
