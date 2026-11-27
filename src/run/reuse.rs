//! Pre-dispatch archived-branch reuse: validate an archived rejected
//! branch against the CURRENT scope + gate and merge it when it passes,
//! so `af run` never re-buys work that is already durable on disk.

use super::UNKNOWN_WORKER;
use crate::config::{Config, Settings, TaskState};
use crate::execute;
use crate::gate;
use crate::state::{AttemptPhase, Receipt, Store, TaskStatus, OUTCOME_RECOVERED};
use crate::worktree;
use std::collections::{HashMap, HashSet};
use std::time::Duration;

/// Operator escape hatch for pre-dispatch reuse: `AF_NO_REUSE=1` (also
/// accepts `true`/`yes`) makes `af run` buy fresh agent work even when a
/// scope-compatible archived branch exists, for an operator who wants a
/// clean re-run. Anything else (including unset) leaves reuse ON — that is
/// the point of the optimisation.
pub(crate) fn reuse_disabled() -> bool {
    matches!(
        crate::config::af_var("AF_NO_REUSE").as_deref(),
        Some("1") | Some("true") | Some("yes")
    )
}

/// Why a pre-dispatch reuse candidate did not let the run skip the agent.
/// Every outcome except `Reused` falls through to the normal dispatch; the
/// variants exist so the loop can log the decision without re-deriving it.
enum ReuseOutcome {
    /// The archived branch re-passed scope + gate and is now merged.
    Reused,
    /// The newest archived branch changes files the CURRENT scope forbids.
    OutOfScope,
    /// The newest archived branch failed the acceptance gate.
    GateFailed,
    /// A git/worktree error prevented re-validation.
    Error(String),
}

/// Record the `recovered` receipt for an archived rejected branch that
/// `af recover` or pre-dispatch reuse just merged (`r14-recovery-ledger`).
///
/// The marker carries the ARCHIVED ATTEMPT's number parsed from the branch
/// name so `af cost` can pair it with the failed receipt it reclaims, names
/// the failed receipt's worker and model when that receipt is still on disk
/// (`unknown` otherwise, never inventing history), and measures 0.0s — no
/// agent ran, so recovery spends no new time. The outcome is a NON-VERDICT:
/// `counts_as_verdict` is false for it, so it never enters `WINS/TOTAL` or
/// `MEAN_S`. Nothing here rewrites or deletes the failed receipt; the marker
/// is a new, separate record and history stays append-only.
pub(crate) fn append_recovered_receipt(store: &Store, st: &Settings, task_id: &str, branch: &str) {
    // `attempt == 0` for a legacy archive whose name carries no attempt; such
    // a marker can pair with nothing, but it is still honest about the merge.
    let attempt = worktree::parse_archived_branch(branch, &st.branch_prefix)
        .map(|a| a.attempt)
        .unwrap_or(0);
    let failed = store
        .load_receipts()
        .into_iter()
        .find(|r| r.task == task_id && r.attempt == attempt && r.outcome == "failed");
    let (worker, model) = match failed {
        Some(r) => (r.worker, r.model),
        None => (UNKNOWN_WORKER.to_string(), UNKNOWN_WORKER.to_string()),
    };
    let _ = store.append_receipt(&Receipt {
        task: task_id.to_string(),
        attempt,
        worker,
        model,
        wall_clock_s: 0.0,
        tokens: None,
        turns: None,
        prompt_tokens: None,
        completion_tokens: None,
        ts: crate::state::now_ts(),
        outcome: OUTCOME_RECOVERED.to_string(),
        error: None,
        cost_micros: None,
        contracts: None,
        failure_class: None,
    });
}

/// Re-validate ONE archived rejected branch against the task's CURRENT scope
/// and gate and merge it when it passes — the pre-dispatch twin of
/// [`recover`]. The gate remains the SOLE arbiter, so this can only ever save
/// money, never accept work a fresh attempt would have had to redo.
///
/// On success the task is marked `Done`/`GatePassed` in `status` (the run
/// loop's in-memory map is the source of truth, so the caller's later saves
/// cannot clobber it), the archived branch is consumed, and a non-verdict
/// `recovered` receipt is appended so `af cost` can pair it with the failed
/// attempt it reclaims. On every failure the temporary worktree is removed
/// but the archived branch is LEFT ALONE for `af clean` or a later
/// `af recover`, and no receipt is written (nothing was merged).
fn reuse_archived_branch(
    cfg: &Config,
    st: &Settings,
    merge_locks: &worktree::MergeLocks,
    task: &crate::config::Task,
    branch: &str,
    status: &mut HashMap<String, TaskStatus>,
) -> ReuseOutcome {
    let repo = cfg.repo_dir_for(task, &st.repo_dir);
    let base_branch = match worktree::current_branch(&repo) {
        Ok(b) => b,
        Err(e) => return ReuseOutcome::Error(e),
    };
    let wt = match worktree::attach_existing(&repo, &st.worktree_root, &task.id, branch) {
        Ok(w) => w,
        Err(e) => return ReuseOutcome::Error(e),
    };
    // (a) SCOPE: every path the archived branch changes relative to the merge
    // base must be covered by the task's CURRENT scope (the operator widened
    // it). Reuses the SAME helpers the attempt path enforces with.
    let changed = match execute::changed_paths(&wt.path, &base_branch) {
        Ok(c) => c,
        Err(e) => {
            worktree::remove_worktree_only(&repo, &wt);
            return ReuseOutcome::Error(e);
        }
    };
    if !execute::scope_violations(&changed, &task.scope).is_empty() {
        worktree::remove_worktree_only(&repo, &wt);
        return ReuseOutcome::OutOfScope;
    }
    // (b) GATE: unless the task is manual, re-run the acceptance gate exactly
    // as the attempt path does (including the task's `gate_replay`). Nothing
    // is merged that the CURRENT gate does not pass.
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
                worktree::remove_worktree_only(&repo, &wt);
                return ReuseOutcome::GateFailed;
            }
        }
    }
    // Success: merge with the standard message, mark the task done, persist,
    // and consume the archived branch (its content now lives in the base).
    let msg = format!("af: {} \u{2014} {}", task.id, task.title);
    if let Err(e) = worktree::merge(&repo, branch, merge_locks, &msg) {
        worktree::remove_worktree_only(&repo, &wt);
        return ReuseOutcome::Error(e);
    }
    let store = Store::new(st.state_dir.clone());
    let s = status.entry(task.id.clone()).or_default();
    s.state = TaskState::Done;
    s.last_error = None;
    s.phase = Some(AttemptPhase::GatePassed);
    let _ = store.save(status);
    append_recovered_receipt(&store, st, &task.id, branch);
    worktree::remove(&repo, &wt);
    ReuseOutcome::Reused
}

/// Pre-dispatch reuse decision for one ready task: `true` means the task's
/// work was recovered from an archived branch and the caller must skip the
/// agent dispatch. A SPECIFIC archive is judged at most once per run (an
/// out-of-scope or gate-failing candidate is never re-judged in a loop, and
/// its WHY line is logged once), but the TASK stays reconsiderable whenever
/// a NEWER archive appears in the same run — in particular an archive
/// produced by a failed attempt in this very run (a native failure archives
/// its work before cleanup, and the retry re-dispatch must re-validate it
/// instead of re-buying the work from a fresh agent). The one exception is
/// a task whose LAST attempt died at the acceptance gate: that work was
/// already judged by the gate this run, so revalidating its archive would
/// loop the same rejected work through the gate and starve the fresh-attempt
/// fallback (scheduling spec) — the task's same-run consideration is burned
/// and a fresh agent spawns instead (see the gate check below).
pub(crate) fn reuse_before_dispatch(
    cfg: &Config,
    st: &Settings,
    merge_locks: &worktree::MergeLocks,
    task: &crate::config::Task,
    status: &mut HashMap<String, TaskStatus>,
    considered: &mut HashSet<String>,
) -> bool {
    if matches!(
        status.get(&task.id).map(|s| &s.state),
        Some(TaskState::Done)
    ) {
        // Defensive: a done task is never re-dispatched or reused.
        return false;
    }
    // GATE-FAILURE FALLBACK (scheduling spec — regression canary): align the
    // reason the LAST attempt failed with the reuse decision. A task whose
    // last_error names the acceptance gate died at the GATE; its work was
    // already judged (and rejected) by the gate this run, so revalidating
    // the same-run archive it left behind would re-judge the same rejected
    // work in a loop and starve the fresh-attempt fallback that follows a
    // SECOND gate failure. Burn the task's same-run consideration (the OLD,
    // task-id-keyed behaviour) and decline reuse so the next attempt is a
    // FRESH agent run. Native failures ('turn cap', 'agent exited', timeout,
    // provider error) carry NO such message — their work was never
    // gate-verified, so archive-branch-name reuse is exactly what saves
    // money. No last_error (first dispatch) keeps today's semantics: a
    // pre-seeded archive may be reused without a failing attempt.
    if status
        .get(&task.id)
        .and_then(|s| s.last_error.as_deref())
        .is_some_and(|e| e.contains("acceptance gate failed"))
    {
        considered.insert(task.id.clone());
        return false;
    }
    let repo = cfg.repo_dir_for(task, &st.repo_dir);
    let branches = worktree::archived_branches(&repo, &st.branch_prefix);
    let Some(branch) = worktree::newest_archived_branch(&branches, &st.branch_prefix, &task.id)
    else {
        // No archive exists (YET). This is NOT a judgment against the task:
        // a failed attempt later in this same run may archive its work, and
        // that archive must be reconsidered. Nothing is recorded, so the
        // task stays reconsiderable.
        println!(
            "  \u{21ba} {} no archived branch to reuse (searched {}/{}-rejected-<ts>); dispatching agent",
            task.id, st.branch_prefix, task.id
        );
        return false;
    };
    // The consideration is keyed on the ARCHIVE BRANCH NAME, not the task:
    // re-judging the SAME archive in a loop is forbidden, but arriving at a
    // NEWER archive (fresh branch name) re-opens the decision for the task.
    if !considered.insert(branch.clone()) {
        // Already judged this exact archive this run: never re-judge it.
        return false;
    }
    match reuse_archived_branch(cfg, st, merge_locks, task, &branch, status) {
        ReuseOutcome::Reused => {
            println!(
                "\u{21ba} {} reused archived branch {} (scope allows it; gate re-run, agent not re-run)",
                task.id, branch
            );
            true
        }
        ReuseOutcome::OutOfScope => {
            println!(
                "  \u{21ba} {} archived branch {} rejected: out of scope; dispatching agent",
                task.id, branch
            );
            false
        }
        ReuseOutcome::GateFailed => {
            println!(
                "  \u{21ba} {} archived branch {} rejected: gate failed; dispatching agent",
                task.id, branch
            );
            false
        }
        ReuseOutcome::Error(e) => {
            println!(
                "  \u{21ba} {} archived branch {} not reused: {}; dispatching agent",
                task.id, branch, e
            );
            false
        }
    }
}
