//! `af clean` and `af dry-run`: planning and cleanup commands.

use super::cost::{
    consumed_tokens, consumed_wall_clock_s, token_budget_exhausted, wall_clock_budget_exhausted,
};
use crate::config::{Config, Settings, TaskState};
use crate::scheduler;
use crate::state::Store;
use crate::worktree;
use std::path::PathBuf;

/// `af clean [--dry-run]`: remove orphaned worktrees + branches left behind
/// by crashed runs. Tasks still marked Running are preserved. `--dry-run`
/// reports what would go and changes nothing. Returns the process exit code.
pub fn clean(cfg: &Config, st: &Settings, dry_run: bool) -> i32 {
    let status = Store::new(st.state_dir.clone()).load();
    let running: Vec<String> = status
        .iter()
        .filter(|(_, s)| s.state == TaskState::Running)
        .map(|(k, _)| k.clone())
        .collect();
    // Multi-repo (ADR-11): try every configured repo + the default, exactly
    // like the startup self-heal.
    let mut repo_list: Vec<(String, PathBuf)> = cfg
        .repos
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    repo_list.push(("(default)".to_string(), st.repo_dir.clone()));

    let ids = worktree::clean(
        &repo_list,
        &st.worktree_root,
        &st.branch_prefix,
        &running,
        dry_run,
    );
    // Round 11 leaves `<prefix>/<id>-rejected-<ts>[-<n>]` refs behind; sweep
    // them too. The running-task rule is identical to the worktree rule.
    let rejected = worktree::clean_rejected(&repo_list, &st.branch_prefix, &running, dry_run);
    if ids.is_empty() && rejected.is_empty() {
        println!(
            "clean: no orphaned worktrees under {}",
            st.worktree_root.display()
        );
        return 0;
    }
    for id in &ids {
        let path = st.worktree_root.join(id);
        let branch = format!("{}/{}", st.branch_prefix, id);
        if dry_run {
            println!("would remove {} (branch {branch})", path.display());
        } else {
            println!("removed {} (branch {branch})", path.display());
        }
    }
    for branch in &rejected {
        if dry_run {
            println!("would remove branch {branch}");
        } else {
            println!("removed branch {branch}");
        }
    }
    if !ids.is_empty() {
        if dry_run {
            println!(
                "dry run: {} orphan(s) would be removed; nothing changed",
                ids.len()
            );
        } else {
            println!("clean: removed {} orphan(s)", ids.len());
        }
    }
    0
}

pub fn dry_run(cfg: &Config, st: &Settings) -> i32 {
    let depths = scheduler::compute_depths(cfg).unwrap_or_default();
    let store = Store::new(st.state_dir.clone());
    // Same strict read as the real run: the displayed plan must never
    // disagree with what a run would do, so a corrupt ledger exits 2 too.
    let status = match store.load_checked() {
        Ok(m) => m,
        Err(err) => {
            eprintln!("config error: {err}");
            return 2;
        }
    };
    // Full plan, not just the first ready batch: every task in dispatch-wave
    // order (depth ascending, then config order) with its readiness, so a
    // deep DAG is visible end-to-end before anything is dispatched.
    let mut indexed: Vec<(usize, &crate::config::Task)> = cfg.tasks.iter().enumerate().collect();
    indexed.sort_by(|(ia, a), (ib, b)| {
        let da = depths.get(&a.id).copied().unwrap_or(0);
        let db = depths.get(&b.id).copied().unwrap_or(0);
        da.cmp(&db).then(ia.cmp(ib))
    });
    let worker = cfg
        .workers
        .iter()
        .find(|w| w.enabled)
        .map(|w| w.name.as_str())
        .unwrap_or("-");
    println!(
        "Dry run — dispatch plan ({} tasks, {} workers):",
        cfg.tasks.len(),
        cfg.workers.len()
    );
    println!(
        "  {:<12} {:<5} {:<24} {:<8} SCOPE",
        "TASK", "DEPTH", "READINESS", "WORKER"
    );
    let max_attempts = cfg.defaults.max_attempts;
    let mut ready_now = 0usize;
    for (_, t) in &indexed {
        let depth = depths.get(&t.id).copied().unwrap_or(0);
        let readiness = scheduler::readiness_of(t, &status, max_attempts);
        if readiness == scheduler::Readiness::Ready {
            ready_now += 1;
        }
        println!(
            "  {:<12} {:<5} {:<24} {:<8} {}",
            t.id,
            depth,
            readiness.label(),
            worker,
            if t.scope.is_empty() {
                "*".to_string()
            } else {
                t.scope.join(",")
            }
        );
    }
    // Budget (r8-budget-cap + token-budget-ceiling): surface an exhausted
    // ceiling in the plan too — dry-run still creates nothing and still
    // exits 0.
    if st.max_wall_clock_s > 0 {
        let in_scope: Vec<&crate::config::Task> = cfg.tasks.iter().collect();
        let measured = consumed_wall_clock_s(&store.load_receipts(), &in_scope);
        if wall_clock_budget_exhausted(st, measured) {
            println!(
                "  BUDGET: spent {measured:.1}s of {}s cap (AF_MAX_WALL_CLOCK_S) — a run would stop before dispatching new attempts",
                st.max_wall_clock_s
            );
        }
    }
    if st.max_tokens > 0 {
        let in_scope: Vec<&crate::config::Task> = cfg.tasks.iter().collect();
        let spent = consumed_tokens(&store.load_receipts(), &in_scope);
        if token_budget_exhausted(st, spent) {
            println!(
                "  BUDGET: spent {spent} of {} tokens cap (AF_MAX_TOKENS) — a run would stop before dispatching new attempts",
                st.max_tokens
            );
        }
    }
    println!("  ({ready_now} ready now; nothing was created or changed)");
    0
}
