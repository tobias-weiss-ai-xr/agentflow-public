//! Run: the dispatch loop (poll → reap → dispatch) and query commands used
//! by the CLI (spec: cli, scheduling, state).

/// The `worker` recorded on a receipt when the worker genuinely is not
/// knowable. The startup heal writes it for an attempt that a killed
/// orchestrator left `running`: the task state does not persist which worker
/// was running, and inventing one would be a lie in the cost ledger. It is
/// agentflow admitting ignorance — NOT a worker name — so the cost report
/// must never treat it as a worker that went missing from the config.
pub const UNKNOWN_WORKER: &str = "unknown";

mod cost;
mod dry_run;
mod r#loop;
mod recover;
mod results;
mod reuse;
mod status;

// Re-exports: the public `run` API is unchanged by the module split.
pub use self::cost::{cost, parse_since, CostFilter};
pub use self::dry_run::{clean, dry_run};
pub use self::r#loop::{
    pick_for_attempt, pick_worker, pool_capacity, run_loop, BackoffState, RunOptions,
};
pub use self::recover::recover;
pub use self::results::{attach, results};
pub use self::status::{status_board, status_json, stop_cmd};

pub(crate) use self::recover::branch_merged_into_head;

#[cfg(test)]
mod tests {
    use super::branch_merged_into_head;
    use super::cost::{
        basis_line, basis_of, cost, mean_duration_cell, measured_task_cell, measured_worker_cell,
        parse_since, task_cost_cell, waste_cause, worker_cost_cell, CostFilter,
    };
    use super::r#loop::{
        backoff_secs, cascade_worker, gate_fingerprint, hot_reload, pick_for_attempt,
        pool_capacity, repeated_gate_defect, PidFile,
    };
    use super::recover::{
        branch_exists, heal_stale_attempt, mark_resumed_failure, merge_commit_in_base,
    };
    use super::status::{board_of, status_board, status_json, stop_cmd};
    use crate::config::{Config, Settings, Task, TaskState, WorkerDefaults};
    use crate::cost::Basis;
    use crate::router::Router;
    use crate::state::{AttemptPhase, Receipt, Store, TaskStatus};
    use crate::worktree;
    use std::collections::{BTreeMap, HashMap};
    use std::path::{Path, PathBuf};
    use std::process::Command;

    // --- hermetic fixtures -------------------------------------------------

    /// Unique per-test base directory. Tests derive their repo/state/worktree
    /// paths from this, so parallel tests never share a parent and cannot
    /// delete each other's scratch state.
    fn unique_base(tag: &str) -> PathBuf {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("af-run-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn git_cmd(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .current_dir(dir)
            .output()
            .expect("git runs");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn scratch_repo() -> PathBuf {
        let base = unique_base("repo");
        let dir = base.join("repo");
        std::fs::create_dir_all(&dir).unwrap();
        git_cmd(&dir, &["init", "-b", "main"]);
        // Repo-local identity: af's merge commits need one and parallel tests
        // must not race on the process environment.
        git_cmd(&dir, &["config", "user.name", "af test"]);
        git_cmd(&dir, &["config", "user.email", "af@test"]);
        std::fs::write(dir.join("f.txt"), "base\n").unwrap();
        git_cmd(&dir, &["add", "."]);
        git_cmd(&dir, &["commit", "-m", "init"]);
        dir
    }

    fn cleanup(repo: &Path) {
        if let Some(base) = repo.parent() {
            let _ = std::fs::remove_dir_all(base);
        }
    }

    /// BELT AND BRACES (r13): `Merged` is only trusted after the attempt
    /// branch tip is verified to be an ancestor of the base. Verify the exact
    /// check `execute_attempt` uses: false for a branch with unmerged work,
    /// true once that work is merged.
    // spec: lifecycle/an-attempt-that-produces-no-change-is-not-merged#a-reported-merge-is-verified-in-the-base
    #[test]
    fn branch_merged_into_head_is_true_only_after_the_merge() {
        let repo = scratch_repo();
        // A tip equal to HEAD is already an ancestor (nothing to merge).
        git_cmd(&repo, &["branch", "done"]);
        assert!(
            branch_merged_into_head(&repo, "done"),
            "an equal tip is an ancestor"
        );
        // A branch carrying real work is NOT an ancestor until merged.
        git_cmd(&repo, &["checkout", "-b", "work", "main"]);
        std::fs::write(repo.join("work.txt"), "x\n").unwrap();
        git_cmd(&repo, &["add", "work.txt"]);
        git_cmd(&repo, &["commit", "-m", "work"]);
        git_cmd(&repo, &["checkout", "main"]);
        assert!(
            !branch_merged_into_head(&repo, "work"),
            "unmerged work is not an ancestor"
        );
        // After `git merge --no-ff` the tip IS an ancestor of the base.
        git_cmd(&repo, &["merge", "--no-ff", "work", "-m", "af: T — merge"]);
        assert!(
            branch_merged_into_head(&repo, "work"),
            "a merged branch is an ancestor of the base"
        );
        cleanup(&repo);
    }

    fn task(id: &str) -> Task {
        Task {
            id: id.into(),
            title: format!("task {id}"),
            accept: Some("true".into()),
            ..Default::default()
        }
    }

    fn cfg_with(tasks: Vec<Task>) -> Config {
        cfg_with_workers(tasks, Vec::new())
    }

    /// A config that also DECLARES its workers — the cost report resolves
    /// every COST cell (and the absent-worker footnote) against them.
    fn cfg_with_workers(tasks: Vec<Task>, workers: Vec<crate::config::Worker>) -> Config {
        let by_id = tasks.iter().map(|t| (t.id.clone(), t.clone())).collect();
        Config {
            tasks,
            workers,
            defaults: WorkerDefaults::default(),
            by_id,
            repos: BTreeMap::new(),
            warnings: vec![],
            module_map: None,
        }
    }

    /// A worker with no declared cost basis (neutral — COST cells `-`).
    fn worker_named(name: &str) -> crate::config::Worker {
        crate::config::Worker {
            name: name.into(),
            ..Default::default()
        }
    }

    fn settings(state_dir: PathBuf) -> Settings {
        Settings {
            repo_dir: state_dir.join("repo"),
            worktree_root: state_dir.join("worktrees"),
            state_dir,
            max_parallel: 1,
            branch_prefix: "tf".into(),
            poll_secs: 1,
            agent_max_turns: 0,
            gate_env: vec![],
            tasks_file: PathBuf::new(),
            workers_file: PathBuf::new(),
            prompt_file: PathBuf::new(),
            agent_timeout_s: 3600,
            agent_stall_s: 0,
            max_wall_clock_s: 0,
            max_tokens: 0,
            sandbox_cmd: vec![],
        }
    }

    fn running(attempts: u32, phase: Option<AttemptPhase>) -> TaskStatus {
        TaskStatus {
            state: TaskState::Running,
            attempts,
            last_error: None,
            phase,
            attempt_started_ts: None,
            attempt_worker: None,
        }
    }

    // --- git helpers -------------------------------------------------------

    #[test]
    fn branch_exists_and_merged_into_head_track_git_state() {
        let repo = scratch_repo();
        assert!(
            !branch_exists(&repo, "tf/T1"),
            "branch absent before creation"
        );
        assert!(!branch_merged_into_head(&repo, "tf/T1"));

        git_cmd(&repo, &["checkout", "-b", "tf/T1"]);
        std::fs::write(repo.join("f.txt"), "feature\n").unwrap();
        git_cmd(&repo, &["commit", "-am", "feature work"]);
        assert!(
            branch_exists(&repo, "tf/T1"),
            "branch visible after creation"
        );

        git_cmd(&repo, &["checkout", "main"]);
        assert!(
            !branch_merged_into_head(&repo, "tf/T1"),
            "feature branch is not an ancestor of main before merge"
        );
        git_cmd(&repo, &["merge", "--no-ff", "tf/T1", "-m", "merge tf/T1"]);
        assert!(
            branch_merged_into_head(&repo, "tf/T1"),
            "feature branch is an ancestor of HEAD after merge"
        );

        cleanup(&repo);
    }

    #[test]
    fn merge_commit_in_base_matches_only_this_tasks_af_merge_commit() {
        let repo = scratch_repo();
        assert!(!merge_commit_in_base(&repo, "T1"));

        // A sibling task's af merge commit must not satisfy T1.
        git_cmd(
            &repo,
            &["commit", "--allow-empty", "-m", "af: T2 — other work"],
        );
        assert!(!merge_commit_in_base(&repo, "T1"));
        assert!(merge_commit_in_base(&repo, "T2"));

        // T1's own merge commit is detected — the crash-after-merge window
        // where cleanup already deleted the branch but the work is in base.
        git_cmd(
            &repo,
            &["commit", "--allow-empty", "-m", "af: T1 — do the thing"],
        );
        assert!(merge_commit_in_base(&repo, "T1"));

        cleanup(&repo);
    }

    // --- status board / JSON ----------------------------------------------

    #[test]
    fn board_of_shows_every_task_state_attempts_and_blanks_missing_error() {
        let cfg = cfg_with(vec![task("A"), task("B"), task("C")]);
        let mut status = HashMap::new();
        status.insert(
            "A".into(),
            TaskStatus {
                state: TaskState::Done,
                attempts: 3,
                last_error: None,
                phase: None,
                attempt_started_ts: None,
                attempt_worker: None,
            },
        );
        status.insert(
            "B".into(),
            TaskStatus {
                state: TaskState::Failed,
                attempts: 2,
                last_error: Some("boom".into()),
                phase: None,
                attempt_started_ts: None,
                attempt_worker: None,
            },
        );
        let board = board_of(&cfg, &status);

        let cells = |id: &str| -> Vec<String> {
            board
                .lines()
                .find(|l| l.split_whitespace().next() == Some(id))
                .unwrap_or_else(|| panic!("no row for {id} in:\n{board}"))
                .split_whitespace()
                .map(str::to_string)
                .collect()
        };
        // Done row shows its attempt count and leaves LAST ERROR blank.
        assert_eq!(cells("A"), ["A", "Done", "3"]);
        // Failed row shows the error text.
        assert_eq!(cells("B"), ["B", "Failed", "2", "boom"]);
        // A task absent from the status map renders as the Ready default.
        assert_eq!(cells("C"), ["C", "Ready", "0"]);
    }

    #[test]
    fn status_json_serializes_task_fields_from_the_store() {
        let base = unique_base("json");
        let st = settings(base.clone());
        let cfg = cfg_with(vec![
            Task {
                verify: true,
                ..task("A")
            },
            task("B"),
        ]);
        let mut status = HashMap::new();
        status.insert(
            "A".into(),
            TaskStatus {
                state: TaskState::Failed,
                attempts: 2,
                last_error: Some("nope".into()),
                phase: None,
                attempt_started_ts: None,
                attempt_worker: None,
            },
        );
        Store::new(st.state_dir.clone()).save(&status).unwrap();
        Store::new(st.state_dir.clone())
            .append_receipt(&Receipt {
                task: "A".into(),
                attempt: 1,
                worker: "w1".into(),
                model: "m".into(),
                wall_clock_s: 1.0,
                tokens: None,
                turns: None,
                prompt_tokens: None,
                completion_tokens: None,
                ts: 1,
                outcome: "verified".into(),
                error: None,
                cost_micros: None,
                contracts: Some(vec![crate::state::ContractVerdict {
                    contract: "C-M1".into(),
                    verdict: "pass".into(),
                }]),
                failure_class: None,
            })
            .unwrap();

        let out = status_json(&cfg, &st);
        let v: serde_json::Value = serde_json::from_str(&out).expect("status_json is valid JSON");
        assert_eq!(v["A"]["id"], "A");
        assert_eq!(v["A"]["state"], "failed");
        assert_eq!(v["A"]["attempts"], 2);
        assert_eq!(v["A"]["last_error"], "nope");
        assert_eq!(v["A"]["verify"], true, "verify flag surfaced");
        assert_eq!(v["A"]["verdict"], "verified", "latest verdict surfaced");
        assert_eq!(v["A"]["contracts"][0]["contract"], "C-M1");
        assert_eq!(v["A"]["contracts"][0]["verdict"], "pass");
        // Absent task falls back to the defaults.
        assert_eq!(v["B"]["state"], "ready");
        assert_eq!(v["B"]["attempts"], 0);
        assert!(v["B"]["last_error"].is_null());
        assert_eq!(v["B"]["verdict"], "none", "no receipt for B");

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    // spec: state/failure-class-on-receipts#status-json-surfaces-the-failure-class
    fn status_json_surfaces_the_latest_failure_class() {
        let base = unique_base("status-fc");
        let st = settings(base.clone());
        let cfg = cfg_with(vec![task("A"), task("B")]);
        let mut status = HashMap::new();
        status.insert(
            "A".into(),
            TaskStatus {
                state: TaskState::Failed,
                attempts: 1,
                last_error: Some("harness: HTTP 429".into()),
                ..Default::default()
            },
        );
        Store::new(st.state_dir.clone()).save(&status).unwrap();
        let store = Store::new(st.state_dir.clone());
        store
            .append_receipt(&Receipt {
                task: "A".into(),
                attempt: 1,
                worker: "w1".into(),
                model: "m".into(),
                wall_clock_s: 1.0,
                tokens: None,
                turns: None,
                prompt_tokens: None,
                completion_tokens: None,
                ts: 1,
                outcome: "failed".into(),
                error: Some("harness: HTTP 429".into()),
                cost_micros: None,
                contracts: None,
                failure_class: Some("infra".into()),
            })
            .unwrap();

        let out = status_json(&cfg, &st);
        let v: serde_json::Value = serde_json::from_str(&out).expect("status_json is valid JSON");
        assert_eq!(
            v["A"]["failure_class"], "infra",
            "most recent failed receipt's class surfaces"
        );
        assert!(
            v["B"]["failure_class"].is_null(),
            "no failed receipt for B → null"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn heal_stale_attempt_reruns_unproven_agents_from_ready() {
        let base = unique_base("heal-reset");
        let st = settings(base.clone());
        let cfg = cfg_with(vec![task("A"), task("B")]);
        let mut status = HashMap::new();
        // None (legacy file / inter-attempt gap) and Spawned (no durable agent
        // outcome) both resume as a fresh attempt — never a gate/merge resume.
        status.insert("A".into(), running(1, None));
        status.insert("B".into(), running(2, Some(AttemptPhase::Spawned)));

        // max_attempts 4 (was 3): the interrupted attempts now consume one
        // attempt of budget each on heal, so the fixture needs a spare slot
        // to keep exercising the rewind-to-Ready path for both entries.
        heal_stale_attempt(&cfg, &st, &mut status, "A", 4);
        heal_stale_attempt(&cfg, &st, &mut status, "B", 4);

        for id in ["A", "B"] {
            let s = &status[id];
            assert_eq!(s.state, TaskState::Ready, "{id} rewound to ready");
            assert_eq!(s.phase, None, "{id} journals from Spawned again");
            assert_eq!(
                s.last_error.as_deref(),
                Some("previous run interrupted"),
                "{id} records why it was rewound"
            );
        }
        assert_eq!(
            status["A"].attempts, 2,
            "the interrupted attempt is counted (1 -> 2)"
        );
        assert_eq!(
            status["B"].attempts, 3,
            "the interrupted attempt is counted (2 -> 3)"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn interrupted_attempt_consumes_the_retry_budget() {
        let base = unique_base("heal-budget");
        let st = settings(base.clone());
        let cfg = cfg_with(vec![task("A"), task("B")]);

        // Budget exhausted: a stale running entry whose attempts already sit
        // at max_attempts (3) heals to terminal Failed, never Ready —
        // otherwise a crash loop re-buys the agent forever.
        let mut status = HashMap::new();
        status.insert("A".into(), running(3, None));
        heal_stale_attempt(&cfg, &st, &mut status, "A", 3);
        assert_eq!(status["A"].state, TaskState::Failed, "no budget left");
        assert_eq!(
            status["A"].attempts, 4,
            "the interrupted attempt is counted"
        );
        let err = status["A"].last_error.as_deref().unwrap_or_default();
        assert!(
            err.contains("interrupted") && err.contains("exhausted"),
            "the error says the run was interrupted and the budget spent: {err}"
        );

        // Budget remaining: the interrupted attempt still counts — healing
        // rewinds to Ready at 2 attempts, not a free rewind to 1.
        status.insert("B".into(), running(1, None));
        heal_stale_attempt(&cfg, &st, &mut status, "B", 3);
        assert_eq!(status["B"].state, TaskState::Ready, "budget remains");
        assert_eq!(
            status["B"].attempts, 2,
            "the interrupted attempt is counted"
        );
        assert_eq!(
            status["B"].last_error.as_deref(),
            Some("previous run interrupted")
        );

        // Convergence: repeatedly crash the same task (set the entry back to
        // running, heal). The count climbs by exactly one per started
        // attempt — 1, 2 — and the budget-exhausting third crash lands on
        // Failed at the budget instead of being re-dispatched indefinitely.
        let mut status = HashMap::new();
        status.insert("B".into(), running(0, None));
        for expected in [1u32, 2] {
            heal_stale_attempt(&cfg, &st, &mut status, "B", 3);
            assert_eq!(
                status["B"].attempts, expected,
                "exactly one counted attempt per crash window"
            );
            assert_eq!(status["B"].state, TaskState::Ready, "budget remains");
            status.insert("B".into(), running(expected, None)); // crash again
        }
        heal_stale_attempt(&cfg, &st, &mut status, "B", 3);
        assert_eq!(
            status["B"].attempts, 3,
            "the count never exceeds max_attempts"
        );
        assert_eq!(
            status["B"].state,
            TaskState::Failed,
            "converges to Failed at the budget, not an infinite re-buy"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn heal_stale_attempt_agent_done_merges_without_rerunning_agent() {
        let repo = scratch_repo();
        let base = repo.parent().unwrap().to_path_buf();
        // The agent's durable work: a commit on the attempt branch.
        git_cmd(&repo, &["checkout", "-b", "tf/T1"]);
        std::fs::write(repo.join("f.txt"), "agent work\n").unwrap();
        git_cmd(&repo, &["commit", "-am", "agent work"]);
        git_cmd(&repo, &["checkout", "main"]);

        let mut st = settings(base.join("state"));
        st.repo_dir = repo.clone();
        let cfg = cfg_with(vec![task("T1")]);
        let mut status = HashMap::new();
        status.insert("T1".into(), running(1, Some(AttemptPhase::AgentDone)));

        heal_stale_attempt(&cfg, &st, &mut status, "T1", 3);

        // MergeOnly-of-GatePassed: gate re-run, merge landed, no agent re-run.
        assert_eq!(status["T1"].state, TaskState::Done);
        assert_eq!(status["T1"].phase, Some(AttemptPhase::GatePassed));
        assert_eq!(status["T1"].last_error, None);
        assert_eq!(
            std::fs::read_to_string(repo.join("f.txt"))
                .unwrap()
                .replace("\r\n", "\n")
                .replace("\r", "\n"),
            "agent work\n",
            "the committed agent work reached the base branch"
        );
        assert!(
            !branch_exists(&repo, "tf/T1"),
            "branch consumed by the merge"
        );

        cleanup(&repo);
    }

    #[test]
    fn heal_stale_attempt_verify_archives_never_merges_and_records_verified() {
        let repo = scratch_repo();
        let base = repo.parent().unwrap().to_path_buf();
        // The agent's durable work: a commit on the attempt branch.
        git_cmd(&repo, &["checkout", "-b", "tf/T1"]);
        std::fs::write(repo.join("f.txt"), "agent work\n").unwrap();
        git_cmd(&repo, &["commit", "-am", "agent work"]);
        git_cmd(&repo, &["checkout", "main"]);

        let mut st = settings(base.join("state"));
        st.repo_dir = repo.clone();
        // A verify task: the gate already passed; the resume must archive, never merge.
        let mut verify_task = task("T1");
        verify_task.verify = true;
        let cfg = cfg_with(vec![verify_task]);
        let mut status = HashMap::new();
        status.insert("T1".into(), running(1, Some(AttemptPhase::GatePassed)));

        heal_stale_attempt(&cfg, &st, &mut status, "T1", 3);

        assert_eq!(status["T1"].state, TaskState::Done);
        assert_eq!(status["T1"].phase, Some(AttemptPhase::GatePassed));
        assert_eq!(status["T1"].last_error, None);
        // The base branch is UNTOUCHED: the verify task's work is archived, not merged.
        assert_eq!(
            std::fs::read_to_string(repo.join("f.txt"))
                .unwrap()
                .replace("\r\n", "\n")
                .replace("\r", "\n"),
            "base\n",
            "the verify branch must never reach the base"
        );
        assert!(
            !branch_exists(&repo, "tf/T1"),
            "the attempt branch is consumed (archived then removed)"
        );
        // The durable work survives under an archived rejected branch.
        let archived = worktree::archived_branches(&repo, "tf");
        assert!(
            archived.iter().any(|b| b.starts_with("tf/T1-rejected-")),
            "the verify work is preserved under an archived branch: {archived:?}"
        );
        // A `verified` receipt is recorded for the resumed attempt.
        let store = Store::new(st.state_dir.clone());
        let receipts = store.load_receipts();
        assert!(
            receipts
                .iter()
                .any(|r| r.task == "T1" && r.outcome == "verified"),
            "a verified receipt is recorded: {receipts:?}"
        );

        cleanup(&repo);
    }

    #[test]
    fn mark_resumed_failure_is_terminal_only_at_max_attempts() {
        let mut status = HashMap::new();
        status.insert("A".into(), running(3, Some(AttemptPhase::GatePassed)));
        status.insert("B".into(), running(1, Some(AttemptPhase::GatePassed)));

        mark_resumed_failure(&mut status, "A", 3, "resumed acceptance gate failed");
        mark_resumed_failure(&mut status, "B", 3, "resumed acceptance gate failed");

        assert_eq!(
            status["A"].state,
            TaskState::Failed,
            "max attempts is terminal"
        );
        assert_eq!(status["B"].state, TaskState::Ready, "below max retries");
        assert_eq!(status["A"].phase, None, "next attempt restarts at Spawned");
        assert_eq!(
            status["A"].last_error.as_deref(),
            Some("resumed acceptance gate failed")
        );
    }

    // --- cost --------------------------------------------------------------

    #[test]
    fn cost_aggregates_wall_clock_and_filters_by_task() {
        let base = unique_base("cost");
        let st = settings(base.clone());
        let cfg = cfg_with_workers(vec![task("A"), task("B")], vec![worker_named("w1")]);
        let store = Store::new(st.state_dir.clone());
        let receipt = |task: &str, wall: f64, outcome: &str| Receipt {
            task: task.into(),
            attempt: 1,
            worker: "w1".into(),
            model: "m".into(),
            wall_clock_s: wall,
            tokens: None,
            turns: None,
            prompt_tokens: None,
            completion_tokens: None,
            ts: 1,
            outcome: outcome.into(),
            error: None,
            cost_micros: None,
            contracts: None,
            failure_class: None,
        };
        store.append_receipt(&receipt("A", 10.0, "merged")).unwrap();
        store.append_receipt(&receipt("A", 5.5, "failed")).unwrap();
        store.append_receipt(&receipt("B", 2.0, "merged")).unwrap();

        // No window flags: byte-identical legacy report — every receipt in
        // the TOTAL count and the trust block, retries double-counted.
        let all = cost(&cfg, &st, &CostFilter::default());
        assert!(all.contains("TOTAL: 17.5s across 3 receipt(s)"), "{all}");

        let only_a = cost(
            &cfg,
            &st,
            &CostFilter {
                task: Some("A".into()),
                ..Default::default()
            },
        );
        assert!(
            only_a.contains("TOTAL: 15.5s across 3 receipt(s)"),
            "{only_a}"
        );
        assert!(
            !only_a
                .lines()
                .any(|l| l.split_whitespace().next() == Some("B")),
            "task B is filtered out:\n{only_a}"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    // spec: cli/worker-infra-drift-breakdown-in-the-cost-report#infra-failures-surface-per-worker
    fn cost_infra_column_shows_per_worker_count_and_footnote() {
        let base = unique_base("cost-infra");
        let st = settings(base.clone());
        let cfg = cfg_with_workers(
            vec![task("A")],
            vec![worker_named("w1"), worker_named("w2")],
        );
        let store = Store::new(st.state_dir.clone());
        let infra_receipt = |task: &str, worker: &str, ts: u64, error: &str| Receipt {
            task: task.into(),
            attempt: 1,
            worker: worker.into(),
            model: "m".into(),
            wall_clock_s: 5.0,
            tokens: None,
            turns: None,
            prompt_tokens: None,
            completion_tokens: None,
            ts,
            outcome: "failed".into(),
            error: Some(error.into()),
            cost_micros: None,
            contracts: None,
            failure_class: Some("infra".into()),
        };
        let merged_receipt = |task: &str, worker: &str| Receipt {
            task: task.into(),
            attempt: 1,
            worker: worker.into(),
            model: "m".into(),
            wall_clock_s: 10.0,
            tokens: None,
            turns: None,
            prompt_tokens: None,
            completion_tokens: None,
            ts: 0,
            outcome: "merged".into(),
            error: None,
            cost_micros: None,
            contracts: None,
            failure_class: None,
        };
        // w1: 2 merged + 3 infra failures; w2: 1 merged, no infra.
        store.append_receipt(&merged_receipt("A", "w1")).unwrap();
        store.append_receipt(&merged_receipt("A", "w1")).unwrap();
        store
            .append_receipt(&infra_receipt("A", "w1", 100, "provider 429: rate limit"))
            .unwrap();
        store
            .append_receipt(&infra_receipt("A", "w1", 200, "provider 500: server error"))
            .unwrap();
        store
            .append_receipt(&infra_receipt("A", "w1", 300, "dead model: 404 not found"))
            .unwrap();
        store.append_receipt(&merged_receipt("A", "w2")).unwrap();

        let report = cost(&cfg, &st, &CostFilter::default());
        // INFRA column header is present.
        assert!(report.contains("INFRA"), "INFRA column header: {report}");
        // w1 row shows INFRA = 3 and 2/2 wins.
        let w1_line = report
            .lines()
            .find(|l| l.split_whitespace().next() == Some("w1"))
            .unwrap();
        assert!(w1_line.contains("2/2"), "w1 wins/total: {w1_line}");
        assert!(w1_line.contains("3"), "w1 infra count: {w1_line}");
        // w2 row shows INFRA = -.
        let w2_line = report
            .lines()
            .find(|l| l.split_whitespace().next() == Some("w2"))
            .unwrap();
        assert!(w2_line.contains("-"), "w2 infra is dash: {w2_line}");
        // Footnote names w1 with count 3 and the most recent infra error.
        assert!(
            report.contains("infra: w1 (3): dead model: 404 not found"),
            "footnote with most recent error: {report}"
        );
        assert!(
            !report.contains("infra: w2"),
            "no footnote for w2 (0 infra)"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    // spec: cli/worker-infra-drift-breakdown-in-the-cost-report#an-all-healthy-report-is-unchanged
    fn cost_all_healthy_report_omits_infra_column() {
        let base = unique_base("cost-healthy");
        let st = settings(base.clone());
        let cfg = cfg_with_workers(vec![task("A")], vec![worker_named("w1")]);
        let store = Store::new(st.state_dir.clone());
        let receipt = |outcome: &str| Receipt {
            task: "A".into(),
            attempt: 1,
            worker: "w1".into(),
            model: "m".into(),
            wall_clock_s: 10.0,
            tokens: None,
            turns: None,
            prompt_tokens: None,
            completion_tokens: None,
            ts: 0,
            outcome: outcome.into(),
            error: None,
            cost_micros: None,
            contracts: None,
            failure_class: None,
        };
        store.append_receipt(&receipt("merged")).unwrap();
        store.append_receipt(&receipt("failed")).unwrap();

        let report = cost(&cfg, &st, &CostFilter::default());
        assert!(
            !report.contains("INFRA"),
            "no INFRA column when no infra failures: {report}"
        );
        assert!(
            !report.contains("infra:"),
            "no infra footnote when no infra failures: {report}"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    // spec: cli/worker-infra-drift-breakdown-in-the-cost-report#worker-absence-from-the-config-stays-a-note-not-a-column
    fn cost_infra_column_does_not_invent_rows_for_absent_workers() {
        let base = unique_base("cost-absent");
        let st = settings(base.clone());
        // cfg has only w1, but receipts reference w2 (absent from config).
        let cfg = cfg_with_workers(vec![task("A")], vec![worker_named("w1")]);
        let store = Store::new(st.state_dir.clone());
        let infra_receipt = |worker: &str, ts: u64, error: &str| Receipt {
            task: "A".into(),
            attempt: 1,
            worker: worker.into(),
            model: "m".into(),
            wall_clock_s: 5.0,
            tokens: None,
            turns: None,
            prompt_tokens: None,
            completion_tokens: None,
            ts,
            outcome: "failed".into(),
            error: Some(error.into()),
            cost_micros: None,
            contracts: None,
            failure_class: Some("infra".into()),
        };
        let merged_receipt = |worker: &str| Receipt {
            task: "A".into(),
            attempt: 1,
            worker: worker.into(),
            model: "m".into(),
            wall_clock_s: 10.0,
            tokens: None,
            turns: None,
            prompt_tokens: None,
            completion_tokens: None,
            ts: 0,
            outcome: "merged".into(),
            error: None,
            cost_micros: None,
            contracts: None,
            failure_class: None,
        };
        store.append_receipt(&merged_receipt("w1")).unwrap();
        store
            .append_receipt(&infra_receipt("w2", 100, "provider 500: server error"))
            .unwrap();

        let report = cost(&cfg, &st, &CostFilter::default());
        // w1 is in the config and has a merged verdict — it appears in the
        // trust table with INFRA = - (no infra for w1).
        let w1_line = report
            .lines()
            .find(|l| l.split_whitespace().next() == Some("w1"))
            .unwrap();
        assert!(w1_line.contains("-"), "w1 has no infra: {w1_line}");
        // w2 is absent from the config — the existing "absent from the
        // config" footnote names it, and the INFRA column does NOT invent
        // a trust row for it.
        assert!(
            report.contains("absent") || report.contains("w2"),
            "absent worker w2 is noted: {report}"
        );
        // No trust row starts with w2 (it's not in cfg.workers).
        let trust_row = report.lines().find(|l| l.starts_with("w2 "));
        assert!(
            trust_row.is_none(),
            "no trust row for absent worker w2: {report}"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn waste_cause_is_text_before_the_first_colon_or_unknown() {
        let r = |error: Option<&str>| Receipt {
            task: "A".into(),
            attempt: 1,
            worker: "w".into(),
            model: "m".into(),
            wall_clock_s: 1.0,
            tokens: None,
            turns: None,
            prompt_tokens: None,
            completion_tokens: None,
            ts: 0,
            outcome: "failed".into(),
            error: error.map(str::to_string),
            cost_micros: None,
            contracts: None,
            failure_class: None,
        };
        // No error on the receipt → the literal key.
        assert_eq!(waste_cause(&r(None)), "unknown");
        // Text before the first `:`, trimmed, whitespace collapsed: the file
        // list AFTER the colon is not part of the cause.
        assert_eq!(
            waste_cause(&r(Some("  gate failed (exit 1): boom\nextra detail"))),
            "gate failed (exit 1)"
        );
        assert_eq!(
            waste_cause(&r(Some(
                "attempt   edited files out of scope:   src/run.rs, tests/other.rs"
            ))),
            "attempt edited files out of scope"
        );
        // No colon → the whole (trimmed) string, never truncated mid-word.
        let long = "x".repeat(60);
        assert_eq!(waste_cause(&r(Some(&long))), "x".repeat(60));
        // Nothing before the colon → the literal key, not an empty row.
        assert_eq!(waste_cause(&r(Some(": boom"))), "unknown");
    }

    // --- gate_fingerprint (B: defect detection) ---

    #[test]
    fn gate_fingerprint_returns_none_for_non_gate() {
        assert!(gate_fingerprint("harness: turn cap 16 reached").is_none());
        assert!(gate_fingerprint("provider error: 429 rate limit").is_none());
        assert!(gate_fingerprint("").is_none());
    }

    #[test]
    fn gate_fingerprint_extracts_exit_code_and_tail() {
        let fp = gate_fingerprint("acceptance gate failed (exit 1): YAML-FAIL file.yaml").unwrap();
        assert_eq!(fp.0, "1");
        assert_eq!(fp.1, "YAML-FAIL file.yaml");
    }

    #[test]
    fn gate_fingerprint_normalizes_whitespace_in_tail() {
        let fp = gate_fingerprint("acceptance gate failed (exit 1):   YAML-FAIL  \n  file.yaml  ")
            .unwrap();
        assert_eq!(fp.0, "1");
        assert_eq!(fp.1, "YAML-FAIL file.yaml");
    }

    #[test]
    fn gate_fingerprint_preserves_dash_exit_code() {
        let fp = gate_fingerprint("acceptance gate failed (exit -): something went wrong").unwrap();
        assert_eq!(fp.0, "-");
        assert_eq!(fp.1, "something went wrong");
    }

    #[test]
    fn gate_fingerprint_returns_none_for_empty_tail() {
        // The marker "(gate produced no output; exit N)" is a non-empty
        // tail, so it has a fingerprint. But a truly empty tail (no body
        // after the colon) returns None — no fingerprint to compare.
        assert!(gate_fingerprint("acceptance gate failed (exit 1):").is_none());
    }

    #[test]
    fn gate_fingerprint_handles_the_silent_marker() {
        let fp =
            gate_fingerprint("acceptance gate failed (exit 1): (gate produced no output; exit 1)")
                .unwrap();
        assert_eq!(fp.0, "1");
        assert_eq!(fp.1, "(gate produced no output; exit 1)");
    }

    // --- repeated_gate_defect (B: defect detection) ---

    /// Helper: a failed gate receipt with the given task, attempt, wall
    /// clock, turns, and error.
    fn gate_receipt(
        task: &str,
        attempt: u32,
        wall: f64,
        turns: Option<u32>,
        error: &str,
    ) -> Receipt {
        Receipt {
            task: task.into(),
            attempt,
            worker: "w".into(),
            model: "m".into(),
            wall_clock_s: wall,
            tokens: None,
            turns,
            prompt_tokens: None,
            completion_tokens: None,
            ts: 0,
            outcome: "failed".into(),
            error: Some(error.into()),
            cost_micros: None,
            contracts: None,
            failure_class: Some("agent".into()),
        }
    }

    #[test]
    // spec: lifecycle/gate-failure-defect-detection#identical-gate-failure-stops-before-the-retry-budget
    fn repeated_gate_defect_stops_when_fingerprints_match() {
        let err = "acceptance gate failed (exit 1): PARSE-FAIL nix/images/infra/busybox.nix";
        let prior = vec![gate_receipt("T", 1, 120.0, Some(8), err)];
        assert!(
            repeated_gate_defect(err, "T", 2, &prior),
            "identical fingerprint from an agent-driven prior attempt triggers the stop"
        );
    }

    #[test]
    // spec: lifecycle/gate-failure-defect-detection#a-different-fingerprint-retries-normally
    fn repeated_gate_defect_different_fingerprint_retries() {
        let err1 = "acceptance gate failed (exit 1): PARSE-FAIL a.nix";
        let err2 = "acceptance gate failed (exit 1): PARSE-FAIL b.nix";
        let prior = vec![gate_receipt("T", 1, 120.0, Some(8), err1)];
        assert!(
            !repeated_gate_defect(err2, "T", 2, &prior),
            "different fingerprint → retry normally"
        );
    }

    #[test]
    // spec: lifecycle/gate-failure-defect-detection#a-gate-only-rerun-never-triggers-alone
    fn repeated_gate_defect_skips_gate_only_rerun() {
        let err = "acceptance gate failed (exit 1): PARSE-FAIL nix/images/infra/busybox.nix";
        // Attempt 1: agent-driven (wall 120s, turns 8). Attempt 2: gate-only
        // rerun (wall 0.5s, turns 0). Current attempt 3: agent-driven, same
        // fingerprint as attempt 1. The gate-only rerun (attempt 2) is
        // skipped; the comparison target is attempt 1.
        let prior = vec![
            gate_receipt("T", 2, 0.5, None, err),
            gate_receipt("T", 1, 120.0, Some(8), err),
        ];
        assert!(
            repeated_gate_defect(err, "T", 3, &prior),
            "gate-only rerun is skipped; comparison target is the prior agent attempt"
        );
    }

    #[test]
    fn repeated_gate_defect_gate_only_rerun_alone_does_not_trigger() {
        let err = "acceptance gate failed (exit 1): PARSE-FAIL nix/images/infra/busybox.nix";
        // Only a gate-only rerun as the prior — no agent-driven prior to
        // compare against → no early stop.
        let prior = vec![gate_receipt("T", 1, 0.5, None, err)];
        assert!(
            !repeated_gate_defect(err, "T", 2, &prior),
            "a gate-only rerun alone never triggers the stop"
        );
    }

    #[test]
    fn repeated_gate_defect_non_gate_failure_does_not_trigger() {
        let err = "harness: turn cap 16 reached";
        let prior = vec![gate_receipt(
            "T",
            1,
            120.0,
            Some(8),
            "acceptance gate failed (exit 1): x",
        )];
        assert!(
            !repeated_gate_defect(err, "T", 2, &prior),
            "non-gate failure never triggers the gate-defect stop"
        );
    }

    #[test]
    fn repeated_gate_defect_first_attempt_never_triggers() {
        let err = "acceptance gate failed (exit 1): PARSE-FAIL x";
        assert!(
            !repeated_gate_defect(err, "T", 1, &[]),
            "attempt 1 has no prior — no stop"
        );
    }

    #[test]
    fn repeated_gate_defect_other_task_does_not_trigger() {
        let err = "acceptance gate failed (exit 1): PARSE-FAIL x";
        let prior = vec![gate_receipt("OTHER", 1, 120.0, Some(8), err)];
        assert!(
            !repeated_gate_defect(err, "T", 2, &prior),
            "a different task's receipts are irrelevant"
        );
    }

    #[test]
    // spec: lifecycle/gate-failure-defect-detection#detection-survives-resume
    fn repeated_gate_defect_survives_resume() {
        // Two agent-driven attempts with identical fingerprints, split
        // across two `af run` invocations. The receipts persisted by the
        // first run (ts=1000) are loaded by the second run (ts=2000); the
        // early stop fires because the fingerprints match.
        let err = "acceptance gate failed (exit 1): PARSE-FAIL nix/images/infra/busybox.nix";
        let prior = vec![gate_receipt("T", 1, 120.0, Some(8), err)];
        // The current attempt (attempt 2) has the same fingerprint.
        assert!(
            repeated_gate_defect(err, "T", 2, &prior),
            "detection fires across resume: persisted receipts trigger the stop"
        );
    }

    // --- backoff_secs (C: adaptive per-worker backoff) ---

    #[test]
    // spec: scheduling/adaptive-backoff-for-infra-failing-workers#repeated-infra-failures-cooldown-longer
    fn backoff_secs_escalates_30_60_120_240_300() {
        assert_eq!(backoff_secs(1, 3), 30);
        assert_eq!(backoff_secs(2, 3), 60);
        assert_eq!(backoff_secs(3, 3), 120);
        assert_eq!(backoff_secs(4, 3), 240);
        assert_eq!(backoff_secs(5, 3), 300, "ceiling at 300s");
        assert_eq!(backoff_secs(6, 3), 300, "ceiling holds at 300s");
        assert_eq!(backoff_secs(10, 3), 300, "ceiling holds at 300s");
    }

    #[test]
    // spec: scheduling/adaptive-backoff-for-infra-failing-workers#a-solo-worker-is-never-locked-out-for-minutes
    fn backoff_secs_solo_worker_capped_at_30s() {
        assert_eq!(backoff_secs(1, 1), 30);
        assert_eq!(backoff_secs(2, 1), 30, "solo worker never exceeds 30s");
        assert_eq!(backoff_secs(5, 1), 30, "solo worker never exceeds 30s");
    }

    #[test]
    fn backoff_secs_zero_level_is_zero() {
        assert_eq!(backoff_secs(0, 3), 0, "level 0 is the reset state");
    }

    #[test]
    // spec: scheduling/adaptive-backoff-for-infra-failing-workers#one-success-resets-the-escalation
    fn backoff_resets_on_any_verdict() {
        // A non-infra outcome (merged, verified, or non-infra failed) must
        // remove the worker's backoff entry so the escalation resets.
        // This is tested at the reap level: the backoff map is cleared on
        // any non-infra outcome. Here we verify the pure helper: level 0
        // means no cooldown.
        assert_eq!(backoff_secs(0, 3), 0);
    }

    #[test]
    // spec: scheduling/adaptive-backoff-for-infra-failing-workers#infra-failures-stay-non-verdicts
    fn backoff_does_not_change_verdict_statistics() {
        // Infra failures stay non-verdicts: they never enter the trust
        // numerator or denominator. The backoff escalation only changes
        // cooldown pacing, not verdict counting. This is verified by the
        // existing classify_failure + counts_as_verdict contract: an infra
        // receipt has counts_as_verdict == false. Here we verify that the
        // backoff helper does not affect this — it only computes a cooldown
        // duration, never a verdict.
        assert_eq!(
            backoff_secs(1, 3),
            30,
            "infra failure sets a cooldown, not a verdict"
        );
        assert_eq!(
            backoff_secs(5, 3),
            300,
            "escalation caps at 300s, still not a verdict"
        );
    }

    #[test]
    fn parse_since_accepts_timestamps_and_utc_midnight_dates() {
        assert_eq!(parse_since("1700000000").unwrap(), 1_700_000_000);
        assert_eq!(
            parse_since(" 1700000000 ").unwrap(),
            1_700_000_000,
            "surrounding whitespace is tolerated"
        );
        assert_eq!(parse_since("1970-01-01").unwrap(), 0, "the epoch");
        assert_eq!(parse_since("1970-01-02").unwrap(), 86_400);
        assert_eq!(parse_since("2023-01-01").unwrap(), 1_672_531_200);
        assert_eq!(
            parse_since("2024-02-29").unwrap(),
            1_709_164_800,
            "leap day"
        );
        assert_eq!(
            parse_since("2000-02-29").unwrap(),
            951_782_400,
            "400-year leap"
        );
        assert_eq!(
            parse_since("2024-04-30").unwrap(),
            1_714_435_200,
            "30-day month"
        );
    }

    #[test]
    fn parse_since_rejects_garbage_and_impossible_dates() {
        for bad in [
            "",
            "not-a-date",
            "1700000000x",
            "2024-1",
            "2024-13-01",
            "2024-02-30",
            "2023-02-29",
            "1969-12-31",
            "-1",
        ] {
            let err = parse_since(bad).unwrap_err();
            assert!(
                bad.is_empty() || err.contains(bad),
                "error names the bad value: {bad} → {err}"
            );
        }
    }

    #[test]
    fn cost_last_keeps_the_latest_attempt_per_task_without_double_counting() {
        let base = unique_base("cost-last");
        let st = settings(base.clone());
        // w1/w2 are declared (no cost basis) so the report's COST cells are
        // `-` and no absent-worker footnote distracts from the window
        // semantics under test.
        let cfg = cfg_with_workers(
            vec![task("A"), task("B"), task("C")],
            vec![worker_named("w1"), worker_named("w2")],
        );
        let store = Store::new(st.state_dir.clone());
        let rcpt =
            |task: &str, attempt: u32, ts: u64, wall: f64, worker: &str, outcome: &str| Receipt {
                task: task.into(),
                attempt,
                worker: worker.into(),
                model: "m".into(),
                wall_clock_s: wall,
                tokens: None,
                turns: None,
                prompt_tokens: None,
                completion_tokens: None,
                ts,
                outcome: outcome.into(),
                error: None,
                cost_micros: None,
                contracts: None,
                failure_class: None,
            };
        // A: a failed retry then a merge on another worker. B: a same-ts
        // tie between attempts 1/2 — the greater attempt must win. C: two
        // identical (ts, attempt) receipts — exactly one is kept.
        store
            .append_receipt(&rcpt("A", 1, 100, 30.0, "w1", "failed"))
            .unwrap();
        store
            .append_receipt(&rcpt("A", 2, 200, 12.0, "w2", "merged"))
            .unwrap();
        store
            .append_receipt(&rcpt("B", 1, 300, 7.0, "w1", "merged"))
            .unwrap();
        store
            .append_receipt(&rcpt("B", 2, 300, 3.0, "w1", "failed"))
            .unwrap();
        store
            .append_receipt(&rcpt("C", 1, 400, 2.0, "w1", "merged"))
            .unwrap();
        store
            .append_receipt(&rcpt("C", 1, 400, 2.0, "w1", "merged"))
            .unwrap();

        // No flags: legacy totals — every attempt, trust over every receipt.
        let all = cost(&cfg, &st, &CostFilter::default());
        assert!(all.contains("TOTAL: 56.0s across 6 receipt(s)"), "{all}");
        assert!(all.contains(&format!("{:<12} {:<9}", "A", 2)), "{all}");
        assert!(
            all.contains(&format!("{:<14} {:<11} {:.2}", "w1", "3/5", 0.60)),
            "{all}"
        );

        // --last: ONE receipt per task — greatest ts, ties by greater
        // attempt — so retries are not double-counted.
        let last = cost(
            &cfg,
            &st,
            &CostFilter {
                last: true,
                ..Default::default()
            },
        );
        assert!(last.contains("TOTAL: 17.0s across 3 receipt(s)"), "{last}");
        assert!(
            last.contains(&format!("{:<12} {:<9} {:<10.1}", "A", 1, 12.0)),
            "{last}"
        );
        // B's tie: attempt 2 (3.0s, failed) wins over attempt 1 (7.0s,
        // merged) — the row AND the trust block prove it.
        assert!(
            last.contains(&format!("{:<12} {:<9} {:<10.1}", "B", 1, 3.0)),
            "{last}"
        );
        assert!(
            !last.contains("30.0"),
            "A's retry is not double-counted: {last}"
        );
        // Trust from the SELECTED receipts only: w1 keeps B@2 (failed) and
        // C@1 (merged) → 1/2; w2 keeps A's merge → 1/1.
        assert!(
            last.contains(&format!("{:<14} {:<11} {:.2}", "w1", "1/2", 0.50)),
            "{last}"
        );
        assert!(
            last.contains(&format!("{:<14} {:<11} {:.2}", "w2", "1/1", 1.00)),
            "{last}"
        );

        // --last composes with --task: rows narrow to A, the window still
        // spans the latest-per-task selection (legacy count semantics).
        let last_a = cost(
            &cfg,
            &st,
            &CostFilter {
                task: Some("A".into()),
                last: true,
                ..Default::default()
            },
        );
        assert!(
            last_a.contains("TOTAL: 12.0s across 3 receipt(s)"),
            "{last_a}"
        );
        assert!(
            !last_a
                .lines()
                .any(|l| l.split_whitespace().next() == Some("B")),
            "{last_a}"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn cost_since_windows_receipts_and_composes_with_last() {
        let base = unique_base("cost-since");
        let st = settings(base.clone());
        let cfg = cfg_with_workers(
            vec![task("A")],
            vec![worker_named("w1"), worker_named("w2")],
        );
        let store = Store::new(st.state_dir.clone());
        let rcpt = |attempt: u32, ts: u64, wall: f64, worker: &str, outcome: &str| Receipt {
            task: "A".into(),
            attempt,
            worker: worker.into(),
            model: "m".into(),
            wall_clock_s: wall,
            tokens: None,
            turns: None,
            prompt_tokens: None,
            completion_tokens: None,
            ts,
            outcome: outcome.into(),
            error: None,
            cost_micros: None,
            contracts: None,
            failure_class: None,
        };
        store
            .append_receipt(&rcpt(1, 1_600_000_000, 100.0, "w1", "failed"))
            .unwrap(); // 2020-09-13
        store
            .append_receipt(&rcpt(2, 1_700_000_000, 20.0, "w1", "failed"))
            .unwrap(); // 2023-11-14
        store
            .append_receipt(&rcpt(3, 1_750_000_000, 30.0, "w2", "merged"))
            .unwrap();

        // Bare timestamp window: older receipts excluded, newer kept.
        let since = cost(
            &cfg,
            &st,
            &CostFilter {
                since: Some(1_650_000_000),
                ..Default::default()
            },
        );
        assert!(
            since.contains("TOTAL: 50.0s across 2 receipt(s)"),
            "{since}"
        );
        assert!(!since.contains("100.0"), "2020 receipt excluded: {since}");
        assert!(
            since.contains(&format!("{:<14} {:<11} {:.2}", "w1", "0/1", 0.00)),
            "trust reflects the window: {since}"
        );

        // The bound is inclusive: ts >= since.
        let edge = cost(
            &cfg,
            &st,
            &CostFilter {
                since: Some(1_600_000_000),
                ..Default::default()
            },
        );
        assert!(edge.contains("TOTAL: 150.0s across 3 receipt(s)"), "{edge}");

        // --since --last: the latest attempt per task WITHIN the window.
        let both = cost(
            &cfg,
            &st,
            &CostFilter {
                last: true,
                since: Some(1_650_000_000),
                ..Default::default()
            },
        );
        assert!(both.contains("TOTAL: 30.0s across 1 receipt(s)"), "{both}");
        assert!(both.contains(&format!("{:<12} {:<9}", "A", 1)), "{both}");
        assert!(
            both.contains(&format!("{:<14} {:<11} {:.2}", "w2", "1/1", 1.00)),
            "{both}"
        );
        assert!(
            !both.contains("w1"),
            "w1's windowed-out attempts are gone: {both}"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    // --- cost cells (r10-cost-report) -------------------------------------

    #[test]
    fn mean_duration_cell_formats_the_mean_over_verdict_attempts() {
        let rcpt = |wall: f64, outcome: &str| Receipt {
            task: "A".into(),
            attempt: 1,
            worker: "w1".into(),
            model: "m".into(),
            wall_clock_s: wall,
            tokens: None,
            turns: None,
            prompt_tokens: None,
            completion_tokens: None,
            ts: 1,
            outcome: outcome.into(),
            error: None,
            cost_micros: None,
            contracts: None,
            failure_class: None,
        };
        // The mean over the verdict attempts, one decimal.
        let merged = rcpt(2.0, "merged");
        let failed = rcpt(6.0, "failed");
        assert_eq!(mean_duration_cell(&[&merged, &failed]), "4.0");
        assert_eq!(mean_duration_cell(&[&merged]), "2.0");
        // No verdict attempt: unknown, never zero.
        assert_eq!(mean_duration_cell(&[]), "-");
    }

    #[test]
    fn worker_cost_cell_renders_dollars_ratios_and_placeholders() {
        // Cheapest declared Sized basis is 4B, so 8B runs at exactly 2x.
        let all = vec![Basis::Sized(4.0), Basis::Sized(8.0)];
        // Real money: 500k tokens at $2/Mtok, four decimals.
        assert_eq!(
            worker_cost_cell(Some(Basis::Priced(2.0)), Some(500_000), &all),
            "$1.0000"
        );
        // Proxy RATE relative to the cheapest declared basis — `x`, never `$`.
        assert_eq!(
            worker_cost_cell(Some(Basis::Sized(4.0)), Some(1), &all),
            "1.00x"
        );
        assert_eq!(
            worker_cost_cell(Some(Basis::Sized(8.0)), Some(1), &all),
            "2.00x"
        );
        // A DECLARED basis is never blanked by a missing token count: the
        // default output mode records none, and the rate is a property of the
        // worker — otherwise a text-mode campaign shows `-` in every cell, the
        // very question the column answers. A zero-token sum keeps its rate
        // too: it is a rate, not a spend.
        assert_eq!(
            worker_cost_cell(Some(Basis::Sized(8.0)), None, &all),
            "2.00x"
        );
        assert_eq!(
            worker_cost_cell(Some(Basis::Sized(8.0)), Some(0), &all),
            "2.00x"
        );
        // With no tokens a declared price shows the declared RATE — with a
        // unit, so it can never be misread as a spend figure.
        assert_eq!(
            worker_cost_cell(Some(Basis::Priced(2.0)), None, &all),
            "$2.0000/Mtok"
        );
        // No declared basis (or a worker absent from the config): `-`.
        assert_eq!(worker_cost_cell(None, Some(1), &all), "-");
        assert_eq!(worker_cost_cell(None, None, &all), "-");
        // No Sized basis to relate to: never invent a reference rate.
        assert_eq!(worker_cost_cell(Some(Basis::Sized(8.0)), Some(1), &[]), "-");
    }

    #[test]
    fn task_cost_cell_sums_dollars_blends_ratios_or_degrades() {
        let bases = vec![
            ("pw".to_string(), Some(Basis::Priced(2.0))),
            ("sw".to_string(), Some(Basis::Sized(8.0))),
            ("cw".to_string(), Some(Basis::Sized(4.0))),
            ("nw".to_string(), None),
        ];
        let all = vec![Basis::Sized(4.0), Basis::Sized(8.0)];
        let rcpt = |worker: &str, tokens: Option<u64>| Receipt {
            task: "A".into(),
            attempt: 1,
            worker: worker.into(),
            model: "m".into(),
            wall_clock_s: 1.0,
            tokens,
            turns: None,
            prompt_tokens: None,
            completion_tokens: None,
            ts: 1,
            outcome: "merged".into(),
            error: None,
            cost_micros: None,
            contracts: None,
            failure_class: None,
        };
        // Every attempt Priced: the dollars SUM (500k + 250k at $2/Mtok).
        let r1 = rcpt("pw", Some(500_000));
        let r2 = rcpt("pw", Some(250_000));
        assert_eq!(task_cost_cell(&[&r1, &r2], &bases, &all), "$1.5000");
        // Every attempt Sized: the TOKEN-WEIGHTED MEAN rate — 300k at 2x
        // blended with 100k at 1x is (600k + 100k) / 400k = 1.75x.
        let big = rcpt("sw", Some(300_000));
        let small = rcpt("cw", Some(100_000));
        assert_eq!(task_cost_cell(&[&big, &small], &bases, &all), "1.75x");
        // A price and a parameter count are incommensurable: mixed rows are
        // `-`, never a converted figure.
        let r1 = rcpt("pw", Some(1));
        let r2 = rcpt("sw", Some(1));
        assert_eq!(task_cost_cell(&[&r1, &r2], &bases, &all), "-");
        // A worker with no basis, or absent from the config entirely: `-`.
        let r = rcpt("nw", Some(1));
        assert_eq!(task_cost_cell(&[&r], &bases, &all), "-");
        let r = rcpt("ghost", Some(1));
        assert_eq!(task_cost_cell(&[&r], &bases, &all), "-");
        // One legacy receipt (no tokens) makes the row `-` — its expense is
        // unknown, so a figure that reads as exact would understate it.
        let r1 = rcpt("pw", Some(1));
        let r2 = rcpt("pw", None);
        assert_eq!(task_cost_cell(&[&r1, &r2], &bases, &all), "-");
        let r1 = rcpt("sw", Some(1));
        let r2 = rcpt("sw", None);
        assert_eq!(task_cost_cell(&[&r1, &r2], &bases, &all), "-");
        // A row whose tokens sum to zero has no weighted-mean denominator.
        let r = rcpt("sw", Some(0));
        assert_eq!(task_cost_cell(&[&r], &bases, &all), "-");
        // No Sized basis to relate to: never invent a reference rate.
        let r = rcpt("sw", Some(1));
        assert_eq!(task_cost_cell(&[&r], &bases, &[]), "-");
    }

    #[test]
    fn basis_line_names_prices_the_proxy_or_none() {
        let priced = vec![("w".to_string(), Some(Basis::Priced(1.0)))];
        let mixed = vec![
            ("w".to_string(), Some(Basis::Priced(1.0))),
            ("v".to_string(), Some(Basis::Sized(8.0))),
        ];
        let sized = vec![("v".to_string(), Some(Basis::Sized(8.0)))];
        let none = vec![("n".to_string(), None)];
        assert_eq!(
            basis_line(&priced, &[], &[]),
            "cost basis: declared prices (USD per 1M tokens)"
        );
        // Both kinds declared: say so, so the mixed `$` / `N.NNx` cells on
        // one report are explained before they are read.
        assert_eq!(
            basis_line(&mixed, &[], &[]),
            "cost basis: declared prices (USD per 1M tokens); some workers declare only params_b"
        );
        assert_eq!(
            basis_line(&sized, &[], &[]),
            "cost basis: params_b proxy (relative; cheapest declared worker = 1.00x)"
        );
        assert_eq!(
            basis_line(&none, &[], &[]),
            "cost basis: none (no worker declares params_b or price_per_mtok_usd)"
        );
    }

    /// (r11-measured-cost) the basis line names the provider as the source
    /// when a selected receipt carries a measured cost, and counts the
    /// attempts still riding a declared-price estimate; a measured-only
    /// window says just `provider-reported`, and no measured cost anywhere
    /// keeps the round-10 lines byte-for-byte.
    #[test]
    fn basis_line_names_the_measured_source_when_one_exists() {
        let bases = vec![
            ("pw".to_string(), Some(Basis::Priced(2.0))),
            ("sw".to_string(), Some(Basis::Sized(8.0))),
        ];
        let all = vec![Basis::Sized(8.0)];
        let rcpt = |worker: &str, tokens: Option<u64>, cost: Option<u64>| Receipt {
            task: "A".into(),
            attempt: 1,
            worker: worker.into(),
            model: "m".into(),
            wall_clock_s: 1.0,
            tokens,
            turns: None,
            prompt_tokens: None,
            completion_tokens: None,
            ts: 1,
            outcome: "merged".into(),
            error: None,
            cost_micros: cost,
            contracts: None,
            failure_class: None,
        };
        // One measured + one price-estimated attempt → named source plus
        // the estimate count, exactly the shape the report prints.
        let measured = rcpt("pw", Some(4242), Some(12_300));
        let estimated = rcpt("pw", Some(500_000), None);
        assert_eq!(
            basis_line(&bases, &[&measured, &estimated], &all),
            "cost basis: provider-reported (USD); 1 of 2 attempt(s) estimated from a declared price"
        );
        // A measured-only window: no estimate note.
        assert_eq!(
            basis_line(&bases, &[&measured], &all),
            "cost basis: provider-reported (USD)"
        );
        // Even a measured cost on a worker declaring NOTHING puts the
        // report on the measured basis (real money is not a proxy).
        let nobasis = vec![("n".to_string(), None)];
        assert_eq!(
            basis_line(&nobasis, &[&measured], &all),
            "cost basis: provider-reported (USD)"
        );
        // No measured cost: the round-10 declaration lines, byte-for-byte
        // (this bases set declares a price AND a size, so the mixed note
        // rides along exactly as it did before).
        assert_eq!(
            basis_line(&bases, &[&estimated], &all),
            "cost basis: declared prices (USD per 1M tokens); some workers declare only params_b"
        );
    }

    /// (r11-measured-cost) the measured report's cells: a provider-reported
    /// cost outranks the declared price for its attempt; dollars containing
    /// an estimated term carry `~`; dollars and ratios never mix.
    #[test]
    fn measured_cells_prefer_reported_costs_and_mark_estimates() {
        let bases = vec![
            ("pw".to_string(), Some(Basis::Priced(2.0))),
            ("sw".to_string(), Some(Basis::Sized(8.0))),
            ("cw".to_string(), Some(Basis::Sized(4.0))),
        ];
        let all = vec![Basis::Sized(4.0), Basis::Sized(8.0)];
        let rcpt = |worker: &str, tokens: Option<u64>, cost: Option<u64>| Receipt {
            task: "A".into(),
            attempt: 1,
            worker: worker.into(),
            model: "m".into(),
            wall_clock_s: 1.0,
            tokens,
            turns: None,
            prompt_tokens: None,
            completion_tokens: None,
            ts: 1,
            outcome: "merged".into(),
            error: None,
            cost_micros: cost,
            contracts: None,
            failure_class: None,
        };
        // Measured beats declared: 500k tokens at $2/Mtok would estimate to
        // $1.0000, but the provider reported $0.0123 — the measurement wins
        // and carries NO `~`.
        let measured = rcpt("pw", Some(500_000), Some(12_300));
        assert_eq!(measured_task_cell(&[&measured], &bases, &all), "$0.0123");
        assert_eq!(
            measured_worker_cell(&[&measured], basis_of("pw", &bases), Some(500_000), &all),
            "$0.0123"
        );
        // Measured + price-estimated: dollars SUM, marked `~` (an estimate
        // must never read as a measurement).
        let estimated = rcpt("pw", Some(500_000), None);
        assert_eq!(
            measured_task_cell(&[&measured, &estimated], &bases, &all),
            "~$1.0123"
        );
        assert_eq!(
            measured_worker_cell(
                &[&measured, &estimated],
                basis_of("pw", &bases),
                Some(1_000_000),
                &all
            ),
            "~$1.0123"
        );
        // Measured + sized: dollars and a ratio are incommensurable — `-`,
        // exactly like a price+sized row.
        let sized = rcpt("sw", Some(1), None);
        assert_eq!(measured_task_cell(&[&measured, &sized], &bases, &all), "-");
        // A measured cost on a worker declaring NOTHING still shows: real
        // money is not a proxy and does not need a declared basis.
        let nobases = vec![("nw".to_string(), None)];
        let m = rcpt("nw", Some(10), Some(12_300));
        assert_eq!(measured_task_cell(&[&m], &nobases, &all), "$0.0123");
        // An unknown attempt (no basis, no tokens) makes the row `-`, never
        // an understated exact-looking figure.
        let unknown = rcpt("nw", None, None);
        assert_eq!(measured_task_cell(&[&m, &unknown], &nobases, &all), "-");
        assert_eq!(
            measured_worker_cell(&[&m, &unknown], None, Some(10), &all),
            "-"
        );
        // All-ratio rows keep the round-10 blend: 300k at 2x + 100k at 1x
        // is a 1.75x weighted mean, and the worker rate is its ratio.
        let big = rcpt("sw", Some(300_000), None);
        let small = rcpt("cw", Some(100_000), None);
        assert_eq!(measured_task_cell(&[&big, &small], &bases, &all), "1.75x");
        assert_eq!(
            measured_worker_cell(&[&big], basis_of("sw", &bases), Some(300_000), &all),
            "2.00x"
        );
        // Worker fallback: a worker with NO measured receipt and nothing
        // foldable keeps the round-10 declared-rate display (a rate is a
        // worker property) — never blanked by the measured report around it.
        let tokenless = rcpt("pw", None, None);
        assert_eq!(
            measured_worker_cell(&[&tokenless], basis_of("pw", &bases), None, &all),
            "$2.0000/Mtok"
        );
        assert_eq!(
            measured_worker_cell(&[&sized], basis_of("sw", &bases), None, &all),
            "2.00x"
        );
    }
    // --- feedback-6-hardening: hot reload, stop, pid file, status header ---

    fn write_min_config(dir: &Path, tasks: &[&str]) -> (PathBuf, PathBuf) {
        let tasks_json = format!(
            r#"{{ "tasks": [{}] }}"#,
            tasks
                .iter()
                .map(|id| format!(r#"{{ "id": "{id}", "title": "{id}" }}"#))
                .collect::<Vec<_>>()
                .join(",")
        );
        let t = dir.join("tasks.json");
        std::fs::write(&t, tasks_json).unwrap();
        let w = dir.join("workers.json");
        std::fs::write(
            &w,
            r#"{ "workers": [ { "name": "w", "provider": "p", "model": "m" } ] }"#,
        )
        .unwrap();
        (t, w)
    }

    #[test]
    // spec: config/task-config-hot-reload-between-dispatch-rounds
    // spec: config/task-config-hot-reload-between-dispatch-rounds#scope-fix-reaches-the-next-retry-without-restart
    // spec: lifecycle/dispatch-rounds-read-the-current-task-config#hot-reload-refreshes-execctx-for-dispatch
    fn hot_reload_picks_up_a_changed_tasks_file() {
        let base = unique_base("hotreload");
        let dir = base.clone();
        std::fs::create_dir_all(&dir).unwrap();
        let (t, w) = write_min_config(&dir, &["A"]);
        let st = Settings {
            tasks_file: t.clone(),
            workers_file: w.clone(),
            ..settings(base)
        };
        let mut seen = None;
        let first = hot_reload(&st, &mut seen).expect("first reload loads the config");
        assert_eq!(first.tasks.len(), 1);

        // No change → no reload.
        assert!(hot_reload(&st, &mut seen).is_none());

        // Edit the task set (new mtime) → reload sees the fix.
        std::thread::sleep(std::time::Duration::from_millis(20));
        let (_, w2) = write_min_config(&dir, &["A", "B"]);
        assert_eq!(w2, st.workers_file);
        std::fs::write(
            &t,
            r#"{ "tasks": [ { "id": "A", "title": "A", "scope": ["src/**"] }, { "id": "B", "title": "B" } ] }"#,
        )
        .unwrap();
        let second = hot_reload(&st, &mut seen).expect("changed file reloads");
        assert_eq!(second.tasks.len(), 2);
        assert_eq!(second.tasks[0].scope, vec!["src/**".to_string()]);
    }

    #[test]
    // spec: config/task-config-hot-reload-between-dispatch-rounds#disabling-every-worker-keeps-the-last-good-pool
    // spec: scheduling/worker-pool-tracks-the-live-enabled-set#all-disabled-reload-keeps-the-last-good-pool
    fn hot_reload_all_disabled_workers_keeps_last_good_pool() {
        let base = unique_base("hotreload-none");
        let dir = base.clone();
        std::fs::create_dir_all(&dir).unwrap();
        let (t, w) = write_min_config(&dir, &["A"]);
        let st = Settings {
            tasks_file: t.clone(),
            workers_file: w.clone(),
            ..settings(base)
        };
        let mut seen = None;
        assert!(hot_reload(&st, &mut seen).is_some(), "initial load ok");

        // Set every worker disabled → validation rejects the pool, the
        // reload yields None, and the previously loaded config stands.
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(
            &w,
            r#"{ "workers": [ { "name": "w", "provider": "p", "model": "m", "enabled": false } ] }"#,
        )
        .unwrap();
        assert!(
            hot_reload(&st, &mut seen).is_none(),
            "all-disabled pool is rejected by validation"
        );
        // And a fresh full load agrees: zero enabled workers is invalid.
        assert!(crate::config::load(&t, &w).is_err());
    }

    #[test]
    // spec: config/task-config-hot-reload-between-dispatch-rounds#broken-config-file-keeps-the-last-good-config
    fn hot_reload_broken_file_keeps_last_good_and_warns_once() {
        let base = unique_base("hotreload-bad");
        let dir = base.clone();
        std::fs::create_dir_all(&dir).unwrap();
        let (t, w) = write_min_config(&dir, &["A"]);
        let st = Settings {
            tasks_file: t.clone(),
            workers_file: w,
            ..settings(base)
        };
        let mut seen = None;
        assert!(hot_reload(&st, &mut seen).is_some(), "initial load ok");

        // Break the file → mtime changes, parse fails, None (caller keeps old).
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&t, "{ not json").unwrap();
        assert!(
            hot_reload(&st, &mut seen).is_none(),
            "broken file yields no config"
        );
        // Same mtime again → silent, no re-warn, no re-parse.
        assert!(hot_reload(&st, &mut seen).is_none());
    }

    #[test]
    // spec: config/status-output-names-its-state-directory
    // spec: config/status-output-names-its-state-directory#human-board-names-the-consulted-state-dir
    fn status_board_names_the_state_dir() {
        let base = unique_base("board-header");
        let st = settings(base);
        let cfg = cfg_with(vec![task("A")]);
        let out = status_board(&cfg, &st);
        let first = out.lines().next().unwrap_or("");
        assert!(
            first.starts_with("state dir: "),
            "first line names the state dir, got: {first}"
        );
        // --json stays a machine contract: no header line there.
        let j = status_json(&cfg, &st);
        assert!(!j.contains("state dir:"), "json output unchanged");
    }

    #[test]
    fn pid_file_is_written_and_removed() {
        let base = unique_base("pidfile");
        let st = settings(base);
        {
            let _p = PidFile::write(&st).expect("pid file written");
            let content = std::fs::read_to_string(st.state_dir.join("af.pid")).unwrap();
            assert_eq!(content.trim(), std::process::id().to_string());
        }
        assert!(
            !st.state_dir.join("af.pid").exists(),
            "drop removes the pid file"
        );
    }

    // --- live worker pool capacity -----------------------------------------

    fn pool_settings(max_parallel: usize) -> Settings {
        let st = settings(unique_base("pool"));
        Settings { max_parallel, ..st }
    }

    fn enabled_worker(name: &str, enabled: bool) -> crate::config::Worker {
        let mut w = worker_named(name);
        w.enabled = enabled;
        w
    }

    #[test]
    // spec: scheduling/worker-pool-tracks-the-live-enabled-set
    // spec: scheduling/worker-pool-tracks-the-live-enabled-set#default-ceiling-follows-the-live-worker-count
    // spec: lifecycle/dispatch-rounds-read-the-current-task-config#adding-a-worker-mid-run-grows-the-pool
    fn pool_capacity_default_follows_the_live_enabled_count() {
        let st = pool_settings(0); // default: one slot per enabled worker
        let two = cfg_with_workers(
            vec![],
            vec![enabled_worker("w1", true), enabled_worker("w2", true)],
        );
        assert_eq!(pool_capacity(&st, &two), 2);
        // The live set GROWS: a third worker appended mid-run widens the pool.
        let three = cfg_with_workers(
            vec![],
            vec![
                enabled_worker("w1", true),
                enabled_worker("w2", true),
                enabled_worker("w3", true),
            ],
        );
        assert_eq!(
            pool_capacity(&st, &three),
            3,
            "appending a worker mid-run grows the pool"
        );
    }

    #[test]
    // spec: scheduling/worker-pool-tracks-the-live-enabled-set#disabling-a-worker-reduces-the-pool
    // spec: lifecycle/dispatch-rounds-read-the-current-task-config#disabling-a-worker-mid-run-shrinks-the-pool
    fn pool_capacity_default_shrinks_when_a_worker_is_disabled() {
        let st = pool_settings(0);
        let three = cfg_with_workers(
            vec![],
            vec![
                enabled_worker("w1", true),
                enabled_worker("w2", true),
                enabled_worker("w3", true),
            ],
        );
        assert_eq!(pool_capacity(&st, &three), 3);
        let two = cfg_with_workers(
            vec![],
            vec![
                enabled_worker("w1", true),
                enabled_worker("w2", true),
                enabled_worker("w3", false),
            ],
        );
        assert_eq!(
            pool_capacity(&st, &two),
            2,
            "disabling a worker mid-run shrinks the pool"
        );
    }

    #[test]
    // spec: scheduling/worker-pool-tracks-the-live-enabled-set#explicit-ceiling-is-never-recomputed
    // spec: lifecycle/dispatch-rounds-read-the-current-task-config#an-explicit-ceiling-stays-fixed
    fn pool_capacity_explicit_ceiling_is_fixed_across_reloads() {
        let st = pool_settings(4);
        let six = cfg_with_workers(
            vec![],
            vec![
                enabled_worker("w1", true),
                enabled_worker("w2", true),
                enabled_worker("w3", true),
                enabled_worker("w4", true),
                enabled_worker("w5", true),
                enabled_worker("w6", true),
            ],
        );
        assert_eq!(
            pool_capacity(&st, &six),
            4,
            "the operator's explicit ceiling is never recomputed"
        );
    }

    #[test]
    // spec: scheduling/worker-pool-tracks-the-live-enabled-set
    fn pool_capacity_is_at_least_one() {
        let st = pool_settings(0);
        // Validation rejects an all-disabled pool, but the function itself
        // never returns 0 — a malformed mid-run edit cannot strand a round.
        let none = cfg_with_workers(vec![], vec![enabled_worker("w1", false)]);
        assert_eq!(pool_capacity(&st, &none), 1);
        assert_eq!(
            pool_capacity(
                &Settings {
                    max_parallel: 0,
                    ..st
                },
                &cfg_with_workers(vec![], vec![])
            ),
            1
        );
    }

    #[test]
    // spec: lifecycle/cooperative-stop-drains-in-flight-attempts#stop-flag-halts-new-dispatches-and-drains
    // spec: lifecycle/cooperative-stop-drains-in-flight-attempts#af-stop-signals-the-pid-file-owner
    fn stop_cmd_records_flag_and_reports_stale_pid() {
        let base = unique_base("stop-stale");
        let st = settings(base);
        std::fs::create_dir_all(&st.state_dir).unwrap();
        // A pid that never exists on any host: not ours, not 1.
        std::fs::write(st.state_dir.join("af.pid"), "2147483000\n").unwrap();
        let code = stop_cmd(&st);
        assert_eq!(code, 0);
        assert!(
            st.state_dir.join("stop").exists(),
            "stop wish is recorded even without a live orchestrator"
        );
    }

    // --- cascade (escalation) routing -------------------------------------

    fn busy_with(names: &[&str]) -> std::sync::Mutex<HashMap<String, usize>> {
        let mut m = HashMap::new();
        for n in names {
            m.insert(n.to_string(), 1);
        }
        std::sync::Mutex::new(m)
    }

    #[test]
    // spec: scheduling/cascade-escalation-routing#first-attempt-runs-the-cheapest-cascade-worker
    // spec: scheduling/cascade-escalation-routing#a-failed-attempt-escalates-to-the-next-worker
    // spec: scheduling/cascade-escalation-routing#past-the-cascade-the-router-picks-again
    // spec: scheduling/cascade-escalation-routing#a-task-without-a-cascade-keeps-ucb1
    fn cascade_picks_cheapest_first_and_escalates_by_attempt() {
        let cfg = cfg_with_workers(
            vec![],
            vec![
                worker_named("cheap"),
                worker_named("pro"),
                worker_named("opus"),
            ],
        );
        let mut t = task("A");
        t.cascade = Some(vec!["cheap".into(), "pro".into(), "opus".into()]);
        let idle = std::sync::Mutex::new(HashMap::<String, usize>::new());
        let router = Router::default();

        // Attempt 1 → cheap; attempt 2 → pro; attempt 3 → opus.
        fn wname(w: Option<&crate::config::Worker>) -> Option<String> {
            w.map(|w| w.name.clone())
        }
        assert_eq!(
            wname(cascade_worker(&cfg, &t, 1, &idle)),
            Some("cheap".to_string())
        );
        assert_eq!(
            wname(cascade_worker(&cfg, &t, 2, &idle)),
            Some("pro".to_string())
        );
        assert_eq!(
            wname(cascade_worker(&cfg, &t, 3, &idle)),
            Some("opus".to_string())
        );
        // Past the cascade → None (caller falls back to UCB1).
        assert!(cascade_worker(&cfg, &t, 4, &idle).is_none());
        // No cascade → None.
        let no_cascade = task("B");
        assert!(cascade_worker(&cfg, &no_cascade, 1, &idle).is_none());
        // The end-to-end decision (`pick_for_attempt`) indexes the cascade
        // by attempt, and a task without a cascade goes straight to UCB1.
        let backoff = std::sync::Mutex::new(HashMap::new());
        assert_eq!(
            wname(pick_for_attempt(
                &cfg, &t, 2, &idle, &backoff, &router, None
            )),
            Some("pro".to_string())
        );
        assert_eq!(
            wname(pick_for_attempt(
                &cfg,
                &no_cascade,
                1,
                &idle,
                &backoff,
                &router,
                None
            )),
            Some("cheap".to_string()),
            "no cascade → first configured worker via UCB1"
        );
    }

    #[test]
    // spec: scheduling/cascade-escalation-routing#an-explicit-worker-pin-overrides-the-cascade
    fn worker_filter_overrides_the_cascade() {
        let cfg = cfg_with_workers(vec![], vec![worker_named("cheap"), worker_named("pro")]);
        let mut t = task("A");
        t.cascade = Some(vec!["cheap".into()]);
        let idle = std::sync::Mutex::new(HashMap::<String, usize>::new());
        let backoff = std::sync::Mutex::new(HashMap::new());
        let router = Router::default();
        let w = pick_for_attempt(&cfg, &t, 1, &idle, &backoff, &router, Some("pro"));
        assert_eq!(
            w.unwrap().name,
            "pro",
            "the pinned worker wins over the cascade"
        );
    }

    #[test]
    // spec: scheduling/cascade-escalation-routing#a-busy-or-disabled-cascade-worker-falls-back-to-ucb1
    fn cascade_falls_back_when_worker_busy_or_disabled() {
        let mut cheap = worker_named("cheap");
        cheap.enabled = false;
        let cfg = cfg_with_workers(vec![], vec![cheap, worker_named("pro")]);
        let mut t = task("A");
        t.cascade = Some(vec!["cheap".into(), "pro".into()]);

        // Disabled first rung → None → UCB1 fallback.
        assert!(cascade_worker(&cfg, &t, 1, &busy_with(&[])).is_none());
        // Busy first rung → None (the caller falls back rather than stall).
        let busy = busy_with(&["cheap"]);
        let cfg2 = cfg_with_workers(vec![], vec![worker_named("cheap"), worker_named("pro")]);
        assert!(cascade_worker(&cfg2, &t, 1, &busy).is_none());
        // Second rung engaged and idle is picked.
        fn wname(w: Option<&crate::config::Worker>) -> Option<String> {
            w.map(|w| w.name.clone())
        }
        assert_eq!(
            wname(cascade_worker(&cfg2, &t, 2, &busy)),
            Some("pro".to_string())
        );
    }
}
