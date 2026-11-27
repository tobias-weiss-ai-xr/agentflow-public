//! Cost report: the `af cost` ledger (measured cost outranks declared price).

use super::UNKNOWN_WORKER;
use crate::config::{Config, Settings};
use crate::cost::{
    attempt_expense, basis as declared_basis, estimate_usd, size_ratio, Basis, Expense,
};
use crate::state::{Receipt, Store, OUTCOME_INTERRUPTED, OUTCOME_RECOVERED};
use std::collections::{BTreeMap, HashMap, HashSet};

/// Selection window for `af cost` (spec: state/cost-receipts — "aggregate
/// receipts (last run, since date, or per task)"). The three fields
/// compose: `since` keeps only receipts at/after the instant, `last` then
/// keeps only each task's most recent surviving receipt, and `task`
/// narrows the table rows. With every field unset the report covers every
/// loaded receipt — byte-identical to the pre-window-flag `af cost`.
#[derive(Default, Debug, Clone)]
pub struct CostFilter {
    pub task: Option<String>,
    pub last: bool,
    pub since: Option<u64>,
}

/// Parse a `--since` value: a bare unix timestamp (`1700000000`) or a
/// `YYYY-MM-DD` date, interpreted as UTC midnight of that day. The result
/// is the inclusive lower bound for [`Receipt::ts`].
pub fn parse_since(value: &str) -> Result<u64, String> {
    let v = value.trim();
    if let Ok(ts) = v.parse::<u64>() {
        return Ok(ts);
    }
    let bad = || format!("cannot parse '{value}' as a unix timestamp or YYYY-MM-DD date");
    let parts: Vec<&str> = v.split('-').collect();
    let (y, m, d) = match parts.as_slice() {
        [y, m, d] => match (y.parse::<i64>(), m.parse::<u32>(), d.parse::<u32>()) {
            (Ok(y), Ok(m), Ok(d)) => (y, m, d),
            _ => return Err(bad()),
        },
        _ => return Err(bad()),
    };
    if !(1..=12).contains(&m) || d == 0 || d > days_in_month(y, m) {
        return Err(format!("date {value} is not a valid calendar date"));
    }
    let days = days_from_civil(y, m, d);
    if days < 0 {
        return Err(format!("date {value} is before the unix epoch"));
    }
    Ok(days as u64 * 86_400)
}

/// Days since 1970-01-01 for a civil (calendar) date — Howard Hinnant's
/// `days_from_civil`, valid over the whole proleptic Gregorian calendar.
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = (i64::from(m) + 9) % 12; // [0, 11]
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

/// Length of month `m` in year `y` (callers validate `m` first; the
/// catch-all answers 31 for January, March, May, July, August, October,
/// December — and any month outside 1-12).
fn days_in_month(y: i64, m: u32) -> u32 {
    match m {
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(y) => 29,
        2 => 28,
        _ => 31,
    }
}

fn is_leap_year(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

/// Apply the `--since` / `--last` window to loaded receipts. `since` keeps
/// receipts with `ts >= since` (applied FIRST, so `--last --since D` means
/// "the latest attempt per task since D"); `last` keeps, per task, only
/// the receipt with the greatest `ts` — ties broken by the greater
/// `attempt` — so retries are never double-counted. The `task` filter is
/// deliberately NOT applied here: it narrows the table rows only, exactly
/// as it did before the window flags existed.
fn select_receipts<'a>(receipts: &'a [Receipt], filter: &CostFilter) -> Vec<&'a Receipt> {
    let mut window: Vec<&Receipt> = match filter.since {
        Some(ts) => receipts.iter().filter(|r| r.ts >= ts).collect(),
        None => receipts.iter().collect(),
    };
    if filter.last {
        // `window` is sorted by `ts` (load_receipts), but ties are broken
        // by `attempt`, so selection is independent of scan order.
        let mut latest: HashMap<&str, &Receipt> = HashMap::new();
        for r in &window {
            let keep = match latest.get(r.task.as_str()) {
                Some(cur) => (r.ts, r.attempt) > (cur.ts, cur.attempt),
                None => true,
            };
            if keep {
                latest.insert(r.task.as_str(), r);
            }
        }
        window = latest.into_values().collect();
        window.sort_by_key(|r| r.ts);
    }
    window
}

/// Campaign budget meter (r8-budget-cap): the wall-clock seconds already
/// consumed by the IN-SCOPE tasks — read through the same loader and the
/// same window selection `af cost` uses, so the enforced ceiling is exactly
/// the TOTAL that report shows. The receipts are the persisted counter
/// (they survive restarts), so no second field has to be kept in sync.
pub(crate) fn consumed_wall_clock_s(
    receipts: &[Receipt],
    in_scope: &[&crate::config::Task],
) -> f64 {
    select_receipts(receipts, &CostFilter::default())
        .into_iter()
        .filter(|r| in_scope.iter().any(|t| t.id == r.task))
        .map(|r| r.wall_clock_s)
        .sum()
}

/// Is the campaign's measured wall-clock spend at or above its ceiling?
/// `max_wall_clock_s == 0` disables the ceiling (unlimited, legacy).
pub(crate) fn wall_clock_budget_exhausted(st: &Settings, measured: f64) -> bool {
    st.max_wall_clock_s > 0 && measured >= st.max_wall_clock_s as f64
}

/// Total tokens spent on in-scope tasks, from the same receipt window the
/// wall-clock ceiling and `af cost` use. A receipt with `tokens: None`
/// (interrupted, or a CLI worker without token capture) contributes 0 —
/// `filter_map` handles this naturally. A `failed` attempt that burned tokens
/// contributes them, exactly as the wall-clock ceiling counts a failed
/// attempt's `wall_clock_s`.
pub(crate) fn consumed_tokens(receipts: &[Receipt], in_scope: &[&crate::config::Task]) -> u64 {
    select_receipts(receipts, &CostFilter::default())
        .into_iter()
        .filter(|r| in_scope.iter().any(|t| t.id == r.task))
        .filter_map(|r| r.tokens)
        .sum()
}

/// Is the campaign's measured token spend at or above its ceiling?
/// `max_tokens == 0` disables the ceiling (unlimited, the default).
pub(crate) fn token_budget_exhausted(st: &Settings, consumed: u64) -> bool {
    st.max_tokens > 0 && consumed >= st.max_tokens
}

/// Total tokens spent on ONE task's receipts (for the per-task ceiling).
/// Same window/filter as the campaign ceiling, scoped to a single task.
pub(crate) fn consumed_tokens_for_task(receipts: &[Receipt], task_id: &str) -> u64 {
    select_receipts(receipts, &CostFilter::default())
        .into_iter()
        .filter(|r| r.task == task_id)
        .filter_map(|r| r.tokens)
        .sum()
}

/// Stable grouping key for a failed attempt's CAUSE: the text before the
/// first `:` in `error`, trimmed, with runs of whitespace collapsed to one
/// space. The full `error` embeds a LIST of files after the colon, so
/// truncating the whole string split one cause across several rows and cut
/// words in half; the prefix groups by cause. An `error` with no colon uses
/// the whole string; `unknown` when the receipt carries no `error` (legacy
/// or failed-without-detail) or nothing precedes the colon.
pub(crate) fn waste_cause(r: &Receipt) -> String {
    match &r.error {
        None => "unknown".to_string(),
        Some(e) => {
            let head = e.split(':').next().unwrap_or("");
            let cause = head.split_whitespace().collect::<Vec<_>>().join(" ");
            if cause.is_empty() {
                "unknown".to_string()
            } else {
                cause
            }
        }
    }
}

/// Per-reason sub-count inside one cause: (full reason, seconds, attempts).
type ReasonCount = (String, f64, usize);
/// One aggregated cause row: (cause, seconds, attempts, per-reason sub-counts).
type CauseAggregate = (String, f64, usize, Vec<ReasonCount>);

/// Every worker in `cfg.workers` paired with its DECLARED basis
/// ([`declared_basis`]): a worker present in the config but declaring
/// nothing keeps an entry (`None`), so it stays distinguishable from a
/// worker ABSENT from the config (no entry at all) — both render `-` in
/// the COST column, but only the absent one is footnoted.
fn worker_bases(cfg: &Config) -> Vec<(String, Option<Basis>)> {
    cfg.workers
        .iter()
        .map(|w| {
            (
                w.name.clone(),
                declared_basis(w.params_b, w.price_per_mtok_usd),
            )
        })
        .collect()
}

/// The DECLARED bases of every worker — the reference set
/// [`size_ratio`] measures a `Sized` worker against. Because it spans
/// EVERY worker in `cfg.workers` (not just those with receipts), the
/// CHEAPEST declaring worker is always exactly `1.00x` in the report,
/// whatever the selected window.
fn declared_bases(bases: &[(String, Option<Basis>)]) -> Vec<Basis> {
    bases.iter().filter_map(|(_, b)| *b).collect()
}

/// The basis a receipt's worker runs on: `None` when the worker declares
/// neither field OR is absent from `cfg.workers` entirely.
pub(crate) fn basis_of(worker: &str, bases: &[(String, Option<Basis>)]) -> Option<Basis> {
    bases
        .iter()
        .find(|(name, _)| name == worker)
        .and_then(|(_, b)| *b)
}

/// The ONE basis line printed above the tables, so a relative proxy can
/// never be misread as money. A MEASURED cost is real money and outranks
/// every declared basis: when any selected receipt carries one, the line
/// names the provider as the source (`provider-reported (USD)`) and counts
/// the attempts that still ride an estimate from a declared price, so a
/// mixed report explains its own `~` markers. With no measured cost the
/// line is chosen from the DECLARATIONS alone — byte-identical to round
/// 10: any worker declaring a price puts the report on the prices basis
/// (with a note when some other worker declares only `params_b`), else the
/// `params_b` proxy, else an honest `none`.
pub(crate) fn basis_line(
    bases: &[(String, Option<Basis>)],
    selected: &[&Receipt],
    all_declared: &[Basis],
) -> String {
    let measured = selected.iter().filter(|r| r.cost_micros.is_some()).count();
    if measured > 0 {
        let mut line = "cost basis: provider-reported (USD)".to_string();
        let estimated = selected
            .iter()
            .filter(|r| {
                matches!(
                    attempt_expense(
                        r.cost_micros,
                        basis_of(&r.worker, bases),
                        r.tokens,
                        all_declared
                    ),
                    Some(Expense::Usd {
                        estimated: true,
                        ..
                    })
                )
            })
            .count();
        if estimated > 0 {
            line.push_str(&format!(
                "; {estimated} of {} attempt(s) estimated from a declared price",
                selected.len()
            ));
        }
        return line;
    }
    let any_priced = bases
        .iter()
        .any(|(_, b)| matches!(b, Some(Basis::Priced(_))));
    let any_sized = bases
        .iter()
        .any(|(_, b)| matches!(b, Some(Basis::Sized(_))));
    if any_priced {
        let mut line = "cost basis: declared prices (USD per 1M tokens)".to_string();
        if any_sized {
            line.push_str("; some workers declare only params_b");
        }
        line
    } else if any_sized {
        "cost basis: params_b proxy (relative; cheapest declared worker = 1.00x)".to_string()
    } else {
        "cost basis: none (no worker declares params_b or price_per_mtok_usd)".to_string()
    }
}

/// The COST cell for one WORKER row. A DECLARED basis always renders
/// something — the rate a worker declares is a property of the WORKER, not
/// of whether its receipts happened to record tokens (the default output
/// mode records none), so a token-less worker must not blank the cell:
///
/// * declared `price_per_mtok_usd` + tokens → real dollars (`$` + 4
///   decimals) for the worker's whole recorded spend, via [`estimate_usd`];
/// * declared `price_per_mtok_usd`, no tokens recorded → the declared RATE,
///   unit-suffixed (`$0.6000/Mtok`) so it can never be misread as a spend;
/// * declared `params_b` → a relative RATE (`N.NNx`, 2 decimals — a PROXY,
///   never a `$`) via [`size_ratio`], whatever the token count;
/// * nothing declared (or absent from `cfg.workers`) → `-`.
pub(crate) fn worker_cost_cell(
    basis: Option<Basis>,
    tokens: Option<u64>,
    all_declared: &[Basis],
) -> String {
    match basis {
        Some(Basis::Sized(params)) => {
            size_ratio(params, all_declared).map_or_else(|| "-".into(), |r| format!("{r:.2}x"))
        }
        Some(Basis::Priced(price)) => match tokens {
            Some(t) => format!("${:.4}", estimate_usd(t, price)),
            None => format!("${price:.4}/Mtok"),
        },
        None => "-".into(),
    }
}

/// The COST cell for one TASK row, which spans every selected attempt of
/// the task — possibly on DIFFERENT workers:
///
/// * every attempt `Priced` → the SUM of the per-attempt dollars;
/// * every attempt `Sized` → the TOKEN-WEIGHTED MEAN rate ratio
///   (Σ tokens·ratio ÷ Σ tokens), the blend the row's tokens actually ran;
/// * anything else — a worker declaring nothing, a receipt naming a worker
///   absent from `cfg.workers`, or a receipt with no `tokens` (legacy) —
///   makes the whole row `-`: one unknown term must not silently
///   understate a figure that reads as exact.
pub(crate) fn task_cost_cell(
    rs: &[&Receipt],
    bases: &[(String, Option<Basis>)],
    all_declared: &[Basis],
) -> String {
    let mut dollars = 0.0; // exact only when every attempt is Priced
    let mut weighted = 0.0; // Σ tokens·ratio — exact only when every attempt is Sized
    let mut tokens = 0.0;
    let mut priced = true;
    let mut sized = true;
    for r in rs {
        match basis_of(&r.worker, bases) {
            Some(Basis::Priced(price)) => {
                sized = false;
                match r.tokens {
                    Some(t) => {
                        dollars += estimate_usd(t, price);
                        tokens += t as f64;
                    }
                    None => return "-".into(),
                }
            }
            Some(Basis::Sized(params)) => {
                priced = false;
                match (r.tokens, size_ratio(params, all_declared)) {
                    (Some(t), Some(ratio)) => {
                        weighted += t as f64 * ratio;
                        tokens += t as f64;
                    }
                    _ => return "-".into(),
                }
            }
            None => return "-".into(),
        }
    }
    if priced {
        format!("${dollars:.4}")
    } else if sized && tokens > 0.0 {
        format!("{:.2}x", weighted / tokens)
    } else {
        "-".into()
    }
}

/// A row's folded expense on a report that carries at least one MEASURED
/// cost: dollars when EVERY attempt folds to dollars (measured or
/// estimated), the token-weighted mean ratio when every attempt folds to
/// a ratio. `None` (rendered `-`) when the attempts are incommensurable —
/// dollars and a parameter ratio must never be converted into each other —
/// or any attempt's expense is unknown (no basis, no tokens, an absent
/// worker): one unknown term must not silently understate a figure that
/// reads as exact.
enum RowExpense {
    Usd { micros: u64, estimated: bool },
    Ratio(f64),
}

fn row_expense(
    rs: &[&Receipt],
    basis_of_attempt: impl Fn(&Receipt) -> Option<Basis>,
    all_declared: &[Basis],
) -> Option<RowExpense> {
    let mut micros: u64 = 0;
    let mut estimated = false;
    let mut any_usd = false;
    let mut weighted = 0.0; // Σ tokens·ratio
    let mut tokens = 0.0;
    let mut any_ratio = false;
    for r in rs {
        match attempt_expense(r.cost_micros, basis_of_attempt(r), r.tokens, all_declared)? {
            Expense::Usd {
                micros: m,
                estimated: e,
            } => {
                any_usd = true;
                micros = micros.saturating_add(m);
                estimated |= e;
            }
            // The ladder guarantees tokens here; a defensive 0 weight can
            // only understate a proxy, never invent one.
            Expense::Ratio(x) => {
                any_ratio = true;
                let t = r.tokens.unwrap_or(0) as f64;
                weighted += t * x;
                tokens += t;
            }
        }
    }
    // A measured dollar amount and a parameter RATIO are incommensurable.
    if any_usd && any_ratio {
        return None;
    }
    if any_usd {
        Some(RowExpense::Usd { micros, estimated })
    } else if tokens > 0.0 {
        Some(RowExpense::Ratio(weighted / tokens))
    } else {
        None
    }
}

/// Render one folded [`RowExpense`] as a COST cell: measured dollars carry
/// no marker; dollars containing ANY estimated term carry `~` so an
/// assumption is never presented as a measurement; a ratio is `N.NNx`,
/// never a `$`.
fn render_row_expense(e: &RowExpense) -> String {
    match e {
        RowExpense::Usd { micros, estimated } => format!(
            "{}${:.4}",
            if *estimated { "~" } else { "" },
            *micros as f64 / 1_000_000.0
        ),
        RowExpense::Ratio(r) => format!("{r:.2}x"),
    }
}

/// The COST cell for one TASK row on a measured report — same position and
/// placeholder rules as [`task_cost_cell`], but folded through the
/// truthfulness ladder ([`row_expense`]) so a provider-reported cost
/// outranks the declared basis for its own attempt.
pub(crate) fn measured_task_cell(
    rs: &[&Receipt],
    bases: &[(String, Option<Basis>)],
    all_declared: &[Basis],
) -> String {
    row_expense(rs, |r| basis_of(&r.worker, bases), all_declared)
        .as_ref()
        .map_or_else(|| "-".into(), render_row_expense)
}

/// The COST cell for one WORKER row on a measured report. Same ladder as
/// the task rows — a worker's measured receipts sum as dollars, mixed with
/// an estimate they are `~`-marked — with one worker-specific fallback: a
/// worker none of whose receipts carries a measured cost and none of which
/// folds keeps round 10's DECLARED-RATE display (a declared rate is a
/// property of the worker, not of the tokens that happened to be
/// recorded — see [`worker_cost_cell`]).
pub(crate) fn measured_worker_cell(
    rs: &[&Receipt],
    basis: Option<Basis>,
    tokens: Option<u64>,
    all_declared: &[Basis],
) -> String {
    match row_expense(rs, |_| basis, all_declared) {
        Some(e) => render_row_expense(&e),
        None => {
            if rs.iter().all(|r| r.cost_micros.is_none()) {
                worker_cost_cell(basis, tokens, all_declared)
            } else {
                "-".into()
            }
        }
    }
}

/// The MEAN_S cell for one WORKER row: the mean wall-clock seconds over
/// the worker's VERDICT attempts in the selection — the same statistic the
/// router's duration tie-break reads ([`Router::mean_duration_s`]),
/// printed so the choice is inspectable. ONE decimal; `-` when the worker
/// has no verdict attempt in the selection (no duration is never a zero
/// duration — a row itself only exists once one does).
pub(crate) fn mean_duration_cell(verdicts: &[&Receipt]) -> String {
    if verdicts.is_empty() {
        "-".into()
    } else {
        format!(
            "{:.1}",
            verdicts.iter().map(|r| r.wall_clock_s).sum::<f64>() / verdicts.len() as f64
        )
    }
}

/// `af cost`: produce the cost/ledger report.
///
/// Generates a human-readable report with:
///
/// **Per-task table**: TASK, ATTEMPTS, WALL_S, TOKENS, COST, MODEL
///
/// **Trust block**: WORKER, WINS/TOTAL, TRUST (percentage), MEAN_S, COST
///   - Only `merged`/`failed` outcomes count as verdicts
///   - `interrupted` and `recovered` are excluded from trust calculations
///
/// **Summary lines**:
///   - `TOTAL: Xs across N receipt(s)`
///   - `WASTED: Xs on N of M attempt(s) (Y%)` (failed attempts only)
///   - `RECOVERED: N attempt(s) — Xs of failed work reclaimed` (round 14)
///   - `INTERRUPTED: Xs on N attempt(s)` (orchestrator killed mid-attempt)
///   - `WASTED BY REASON` (grouped by failure cause)
///
/// **Cost model**: Measured provider-reported cost (via `cost_micros`) is
/// shown as `$X.XXXX`. Estimated cost from declared basis is shown as
/// `~$X.XXXX`. Pure size ratio (Sized basis) is shown as `Y.XXx`.
///
/// **Filtering**:
///   - `--task ID`: only show receipts for task ID
///   - `--last`: only the latest attempt per task
///   - `--since DATE|UNIX_TS`: only receipts with `ts >= timestamp`
pub fn cost(cfg: &Config, st: &Settings, filter: &CostFilter) -> String {
    let store = Store::new(st.state_dir.clone());
    // Checked loader: a torn receipt is reported (one `warning:` line per
    // unreadable file, naming it and the parse problem) but never blocks the
    // report — the readable history is still totaled. With no torn files the
    // output is byte-identical to the legacy report.
    let (receipts, problems) = store.load_receipts_checked();
    let selected = select_receipts(&receipts, filter);
    let mut lines = String::new();
    for p in &problems {
        lines.push_str(&format!("warning: {p}\n"));
    }
    // Cost basis (r10-cost-report): one line ABOVE the tables states the
    // units of the COST column before any number is read, so a relative
    // proxy can never be misread as money. `all_declared` spans EVERY
    // worker in cfg.workers, so the cheapest declaring worker is always
    // exactly `1.00x` on the proxy scale. (r11-measured-cost) when any
    // selected receipt carries a provider-reported cost, the same flag
    // switches BOTH tables' cells to the truthfulness ladder — measured
    // money first — and the basis line names the measured source; with no
    // measured cost anywhere the report stays byte-identical to round 10.
    let bases = worker_bases(cfg);
    let all_declared = declared_bases(&bases);
    let report_measured = selected.iter().any(|r| r.cost_micros.is_some());
    lines.push_str(&basis_line(&bases, &selected, &all_declared));
    lines.push('\n');
    lines.push_str(&format!(
        "{:<12} {:<9} {:<10} {:<9} {:<8} {}",
        "TASK", "ATTEMPTS", "WALL_S", "TOKENS", "COST", "MODEL"
    ));
    let mut total: f64 = 0.0;
    for t in &cfg.tasks {
        if let Some(f) = &filter.task {
            if &t.id != f {
                continue;
            }
        }
        let rs: Vec<_> = selected
            .iter()
            .copied()
            .filter(|r| r.task == t.id)
            .collect();
        let wall: f64 = rs.iter().map(|r| r.wall_clock_s).sum();
        total += wall;
        if !rs.is_empty() {
            // Sum the tokens actually recorded; legacy receipts (every one
            // of which carries `None`) show `-` so the legacy report reads
            // exactly as it did before the column existed.
            let mut token_sum: Option<u64> = None;
            for r in &rs {
                if let Some(t) = r.tokens {
                    token_sum = Some(token_sum.unwrap_or(0) + t);
                }
            }
            let tokens = token_sum
                .map(|t| t.to_string())
                .unwrap_or_else(|| "-".into());
            // Estimated expense for the whole row (dollars when every
            // attempt is Priced, the token-weighted mean rate when every
            // attempt is Sized, `-` otherwise). On a measured report the
            // same cell is folded through the ladder instead: a
            // provider-reported cost outranks the declared basis for its
            // own attempt.
            let cost_cell = if report_measured {
                measured_task_cell(&rs, &bases, &all_declared)
            } else {
                task_cost_cell(&rs, &bases, &all_declared)
            };
            lines.push_str(&format!(
                "\n{:<12} {:<9} {:<10.1} {:<9} {:<8} {}",
                t.id,
                rs.len(),
                wall,
                tokens,
                cost_cell,
                rs[0].model
            ));
        }
    }
    lines.push_str(&format!(
        "\nTOTAL: {:.1}s across {} receipt(s)",
        total,
        selected.len()
    ));
    // Per-worker trust (ADR-12): measured win rate from the SELECTED
    // receipt VERDICTS. Interrupted receipts (r9-interrupted) are excluded
    // from wins AND total: they carry no information about the worker.
    let mut by_worker: Vec<(String, u64, u64)> = Vec::new();
    // The worker's summed recorded tokens over the same selected VERDICT
    // receipts that form its row — the input to its COST cell. `None`
    // until a receipt actually carries tokens (legacy receipts never do).
    let mut worker_tokens: HashMap<String, Option<u64>> = HashMap::new();
    // The same receipts themselves, grouped per worker: a measured report
    // folds its COST cell attempt-by-attempt (a provider-reported cost is
    // a per-attempt fact), which the summed tokens alone cannot express.
    let mut worker_receipts: HashMap<String, Vec<&Receipt>> = HashMap::new();
    for r in selected.iter().copied().filter(|r| r.counts_as_verdict()) {
        let e = by_worker.iter_mut().find(|(n, _, _)| n == &r.worker);
        let won = r.is_success();
        match e {
            Some((_, w, n)) => {
                *n += 1;
                if won {
                    *w += 1;
                }
            }
            None => by_worker.push((r.worker.clone(), u64::from(won), 1)),
        }
        let t = worker_tokens.entry(r.worker.clone()).or_insert(None);
        if let Some(v) = r.tokens {
            *t = Some(t.unwrap_or(0) + v);
        }
        worker_receipts.entry(r.worker.clone()).or_default().push(r);
    }
    if !by_worker.is_empty() {
        by_worker.sort();
        // D: per-worker infra-drift column. Count selected failed receipts
        // with failure_class == "infra" per worker, and track the most
        // recent such receipt's error (by ts) for the footnote. When no
        // worker has infra, the column and footnote are omitted entirely so
        // an all-healthy report is byte-identical to today.
        let mut worker_infra: BTreeMap<String, (usize, Option<String>, u64)> = BTreeMap::new();
        for r in &selected {
            if r.outcome == "failed" && r.failure_class.as_deref() == Some("infra") {
                let entry = worker_infra.entry(r.worker.clone()).or_insert((0, None, 0));
                entry.0 += 1;
                if r.ts >= entry.2 {
                    entry.1 = r.error.clone();
                    entry.2 = r.ts;
                }
            }
        }
        let any_infra = worker_infra.values().any(|(c, _, _)| *c > 0);
        if any_infra {
            lines.push_str(&format!(
                "\n\n{:<14} {:<11} {} {} {} {}",
                "WORKER", "WINS/TOTAL", "TRUST", "MEAN_S", "INFRA", "COST"
            ));
        } else {
            lines.push_str(&format!(
                "\n\n{:<14} {:<11} {} {} {}",
                "WORKER", "WINS/TOTAL", "TRUST", "MEAN_S", "COST"
            ));
        }
        for (name, w, n) in by_worker {
            // Estimated expense for the worker's whole recorded token spend
            // on the selected window: dollars from a declared price, a
            // relative rate from `params_b`, `-` when neither is declared,
            // the worker is absent from the config, or no tokens were ever
            // recorded (legacy receipts). On a measured report the cell is
            // folded attempt-by-attempt instead: measured dollars first,
            // `~` when any term is estimated.
            let worker_rs: &[&Receipt] = worker_receipts
                .get(name.as_str())
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            let cost_cell = if report_measured {
                measured_worker_cell(
                    worker_rs,
                    basis_of(&name, &bases),
                    worker_tokens.get(name.as_str()).copied().flatten(),
                    &all_declared,
                )
            } else {
                worker_cost_cell(
                    basis_of(&name, &bases),
                    worker_tokens.get(name.as_str()).copied().flatten(),
                    &all_declared,
                )
            };
            let infra_cell = worker_infra
                .get(name.as_str())
                .map(|(c, _, _)| *c)
                .unwrap_or(0);
            if any_infra {
                lines.push_str(&format!(
                    "\n{:<14} {:<11} {:.2} {} {} {}",
                    name,
                    format!("{w}/{n}"),
                    w as f64 / n as f64,
                    mean_duration_cell(worker_rs),
                    if infra_cell > 0 {
                        infra_cell.to_string()
                    } else {
                        "-".to_string()
                    },
                    cost_cell
                ));
            } else {
                lines.push_str(&format!(
                    "\n{:<14} {:<11} {:.2} {} {}",
                    name,
                    format!("{w}/{n}"),
                    w as f64 / n as f64,
                    mean_duration_cell(worker_rs),
                    cost_cell
                ));
            }
        }
        // Footnote: per-worker infra breakdown (only when any infra exists).
        if any_infra {
            for (name, (count, last_err, _)) in &worker_infra {
                if *count > 0 {
                    let err = last_err
                        .as_deref()
                        .unwrap_or("")
                        .chars()
                        .take(200)
                        .collect::<String>();
                    lines.push_str(&format!("\ninfra: {name} ({count}): {err}"));
                }
            }
        }
    }
    // Wasted spend (failed attempts): computed from the SAME window-selected
    // receipts as the rest of the report, so `--last` / `--since` / `--task`
    // narrow it too. A failed attempt's wall-clock is otherwise
    // indistinguishable from productive spend — this is the only place it is
    // reported.
    //
    // Recovered attempts (r14-recovery-ledger) leave WASTED. A non-verdict
    // `recovered` marker names the archived attempt that `af recover`/reuse
    // merged; the failed receipt it pairs with (same task AND attempt) is NOT
    // wasted — its work landed — so it is removed from the failed set and
    // reported on the RECOVERED line below. Pairing is window-scoped exactly
    // like every other waste figure, so `--last` / `--since` / `--task`
    // narrow it too.
    let recovered: Vec<&Receipt> = selected
        .iter()
        .copied()
        .filter(|r| r.outcome == OUTCOME_RECOVERED)
        .collect();
    let recovered_keys: HashSet<(&str, u32)> = recovered
        .iter()
        .map(|r| (r.task.as_str(), r.attempt))
        .collect();
    let failed: Vec<&Receipt> = selected
        .iter()
        .copied()
        .filter(|r| {
            r.outcome == "failed" && !recovered_keys.contains(&(r.task.as_str(), r.attempt))
        })
        .collect();
    let mut wasted: f64 = 0.0;
    for r in &failed {
        wasted += r.wall_clock_s;
    }
    // The denominator counts real attempts. A `recovered` marker is not a new
    // attempt (no agent ran, 0.0s), so it is excluded; the failed receipt it
    // pairs with stays one attempt whether or not it was later recovered.
    let attempts = selected.len().saturating_sub(recovered.len());
    let pct = if attempts == 0 {
        0.0
    } else {
        failed.len() as f64 / attempts as f64 * 100.0
    };
    lines.push_str(&format!(
        "\nWASTED: {:.1}s on {} of {} attempt(s) ({:.1}%)",
        wasted,
        failed.len(),
        attempts,
        pct
    ));
    // Recovered attempts get their own outcome line, beside WASTED and
    // INTERRUPTED. The reclaimed seconds are the paired failed receipts'
    // wall-clock: work that was paid for once and then merged, so it is not
    // wasted (and, on this report, no longer is).
    if !recovered.is_empty() {
        let reclaimed: f64 = selected
            .iter()
            .copied()
            .filter(|r| {
                r.outcome == "failed" && recovered_keys.contains(&(r.task.as_str(), r.attempt))
            })
            .map(|r| r.wall_clock_s)
            .sum();
        lines.push_str(&format!(
            "\nRECOVERED: {} attempt(s) — {:.1}s of failed work reclaimed (not wasted)",
            recovered.len(),
            reclaimed
        ));
    }
    // Interrupted attempts (r9-interrupted): real spend with an UNKNOWN
    // duration (recorded as 0.0s), so they get their own outcome line
    // instead of being buried in the failed subtotal or silently dropped.
    // They stay out of the trust block above — no verdict on the worker.
    let interrupted: Vec<&Receipt> = selected
        .iter()
        .copied()
        .filter(|r| r.outcome == OUTCOME_INTERRUPTED)
        .collect();
    if !interrupted.is_empty() {
        let secs: f64 = interrupted.iter().map(|r| r.wall_clock_s).sum();
        lines.push_str(&format!(
            "\nINTERRUPTED: {:.1}s on {} attempt(s) — duration unknown (orchestrator exited mid-attempt)",
            secs,
            interrupted.len()
        ));
    }
    if !failed.is_empty() {
        // Primary grouping is by CAUSE (text before the first `:`), never by
        // the full reason: the reason embeds a file LIST, so two failures of
        // the same cause used to land in separate rows. Each cause also keeps
        // its distinct full reasons as an indented sub-count — that is where
        // the file lists stay visible without fragmenting the total.
        type SubCounts = HashMap<String, (f64, usize)>;
        let mut by_cause: HashMap<String, (f64, usize, SubCounts)> = HashMap::new();
        for r in &failed {
            let cause = waste_cause(r);
            let full = r.error.clone().unwrap_or_else(|| "unknown".to_string());
            let entry = by_cause
                .entry(cause)
                .or_insert_with(|| (0.0, 0, HashMap::new()));
            entry.0 += r.wall_clock_s;
            entry.1 += 1;
            let sub = entry.2.entry(full).or_insert((0.0, 0));
            sub.0 += r.wall_clock_s;
            sub.1 += 1;
        }
        let mut rows: Vec<CauseAggregate> = by_cause
            .into_iter()
            .map(|(cause, (secs, n, subs))| {
                let mut sub_rows: Vec<ReasonCount> = subs
                    .into_iter()
                    .map(|(reason, (s, c))| (reason, s, c))
                    .collect();
                // Descending seconds; ties broken by the full reason ascending
                // so the sub-counts are deterministic too.
                sub_rows.sort_by(|a, b| {
                    b.1.partial_cmp(&a.1)
                        .unwrap_or(std::cmp::Ordering::Equal)
                        .then_with(|| a.0.cmp(&b.0))
                });
                (cause, secs, n, sub_rows)
            })
            .collect();
        // Descending seconds; ties broken by the cause string ascending.
        rows.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });
        lines.push_str("\nWASTED BY REASON");
        for (cause, secs, n, subs) in rows {
            lines.push_str(&format!("\n  {:<28} {:.1}s  ({})", cause, secs, n));
            for (reason, rsecs, rn) in subs {
                // When the error carries no colon the cause IS the full
                // reason; a sub-row would only repeat the line above.
                if reason != cause {
                    lines.push_str(&format!("\n    - {} {:.1}s  ({})", reason, rsecs, rn));
                }
            }
        }
    }
    // Footnote (r10-cost-report): receipts naming a worker ABSENT from
    // cfg.workers render as `-`; this line names them (sorted, deduped)
    // with the attempt count. Historical receipts routinely outlive config
    // edits, so this is a NOTE — never an error, never blocking the report.
    let mut unknown: BTreeMap<&str, usize> = BTreeMap::new();
    for r in &selected {
        // `UNKNOWN_WORKER` is the heal's placeholder for "the worker of this
        // killed attempt is unknowable", not a worker that vanished from
        // the config: the INTERRUPTED line already accounts for that
        // attempt. The same holds for an interrupted receipt that names a
        // real worker (r11-attempt-identity: dispatch persists the choice)
        // — an attempt lost to a killed orchestrator is never evidence of
        // a worker missing from the config, measured duration or not.
        if r.counts_as_verdict()
            && r.worker != UNKNOWN_WORKER
            && !bases.iter().any(|(name, _)| name == &r.worker)
        {
            *unknown.entry(r.worker.as_str()).or_insert(0) += 1;
        }
    }
    let unknown_attempts: usize = unknown.values().sum();
    if unknown_attempts > 0 {
        let names = unknown.keys().copied().collect::<Vec<_>>().join(", ");
        lines.push_str(&format!(
            "\nnote: {unknown_attempts} attempt(s) name a worker absent from the config, shown as '-': {names}"
        ));
    }
    lines
}
