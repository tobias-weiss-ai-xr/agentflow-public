# Proposal: classify-on-agent-output

## Why

Receipt forensics on the port-1000 campaign (240 receipts,
14 merged = 5.8%) shows the single largest failure bucket is a
classification bug, not weak models:

- **160 of 167** `failure_class: "agent"` receipts died in **5–7 seconds**
  with `agent exited NonZero (code 1)` on the receipt. The surviving per-task
  logs hold the real cause — `No API key found for openai.` (the pi CLI was
  configured with provider `openai`, so it demanded `OPENAI_API_KEY` the
  sandbox never had) — but that text never reached the receipt, so a
  one-line config error burned 164 attempts and was invisible for a day. `classify_failure` is a keyword match over the receipt reason
  only, so a transport failure is classified `agent`.
- Consequences, all wrong: the UCB1 router lowers trust in workers for
  **gateway outages** (TUD models were routed away from for hours); each
  outage burns one of `max_attempts`; and the ADR-13 retry prompt tells the
  next model "attempt 1: agent exited NonZero (code 1)" — nothing it can
  act on.
- **6 receipts** were SUCCESSFUL 30-minute agent runs killed at merge:
  `worktree::merge` runs `git merge` into the main checkout without checking
  it is clean; a dirty main (e.g. the operator's uncommitted gate output)
  yields `Your local changes … would be overwritten by merge`. The attempt
  is then archived as a rejection even though the agent did nothing wrong.

The existing spec (`state/spec.md` "Failure class on receipts") already
defines `infra` as *"harness, transport, or environment failed — the worker
did not fail"*. The implementation violates that intent for CLI workers
because the transport signal is not in the string it classifies.

Research grounding (`agent-harness-research`, 443 papers):

- **Trial by Trace** (2610.32285) / **Trace-to-Architecture** (2610.16725):
  harness reliability must be judged on the execution trace, and failure
  attribution must separate harness faults from agent faults — "similar
  failures can require different fixes". Classifying on an exit-code
  wrapper instead of the trace is the exact mis-attribution they warn of.
- **The Empire, Long Divided, Must Unite** (2608.23953): the convergent
  harness form is an *append-only replayable record* — the receipt must
  carry enough to replay the diagnosis, which a generic exit code does not.
- **Agent Harness Awareness** (2611.05391): the host holds operational
  state (tool/gateway health) the model does not; it should surface it at
  the decision point — here, the retry prompt and the router.

## What Changes

- **CLI agent diagnostics reach the receipt.** When a CLI worker exits
  non-zero with NO committed work, af folds the agent's CAUSE line into the
  failure reason (`agent exited NonZero (code 1); last output: No API key
  found for openai.`) — the first output line that names an infra signal,
  falling back to the last line. `classify_failure` then returns `infra`.
  The receipt stays short; the full stream is still in the per-task log.
  Native-harness workers already do this (`harness: <e>`).
- **Silent death is named.** Non-zero exit, no commits, and no output at all
  → `agent produced no output (exit N) after Ts` — actionable in a retry
  prompt and in `af status`, instead of a bare exit code.
- **`classify_failure` learns the transport vocabulary** observed in the
  wild: `aborted`, `empty response`, `rate limit`/`429`/`503`, `overloaded`,
  `socket`, `eof`, `reset by peer`, `dns`. Still a single shared function;
  no new dependency.
- **Merge never targets a dirty checkout.** `worktree::merge` pre-flights
  `is_dirty(repo)`; on a dirty tree it returns a clear error
  (`merge target <branch> is dirty: <first file>; commit or stash before
  af merges`) WITHOUT running `git merge` and WITHOUT aborting — nothing to
  abort, and the operator's edits are never touched.
- **Fail fast, not after 30 minutes.** The campaign loop checks the merge
  target for cleanliness BEFORE dispatching each attempt. A dirty target
  parks the dispatch (the task stays pending; the loop re-checks every
  cycle) and logs the reason once. No attempt is consumed. (The merge-time
  guard above remains as the belt to this suspender: an operator can dirty
  the tree mid-attempt.)
- **`af status`** shows the parked reason so the operator sees "fix your
  checkout" rather than a silent stall.

## Impact

- Affected specs: `state` (failure-class requirement wording: the reason
  now carries the agent's last line), `worktree` (merge pre-flight),
  `lifecycle` (dispatch pre-flight / parking).
- Files: `src/execute.rs` (reason builder), `src/worktree.rs` (`merge`),
  `src/run/loop.rs` (dispatch pre-flight), `src/run/status.rs` (surface).
- Backward compatible: old receipts deserialize unchanged; the extra
  `; last output:` suffix is additive; no config changes.
- Research-grounded additions explicitly deferred (ponytail — not needed
  to fix the root cause): cross-task failure recall (ADR-13 keeps
  per-task), automatic gateway health probes, retry-on-infra without
  consuming the attempt budget (see design.md "Deferred").
