# Capability: lifecycle

## ADDED Requirements

### Requirement: Gate-failure defect detection

When a task's most recent AGENT-driven attempt (one that ran the agent, not a gate-only rerun) dies at the acceptance gate, `af run` SHALL compare its gate-error fingerprint to the fingerprint of the previous AGENT-driven gate-failed attempt of the same task. The fingerprint is the pair (exit code, whitespace-normalized tail) of the gate output as recorded on the receipt. When the two fingerprints are IDENTICAL and the task has at least two attempts recorded, `af` SHALL mark the task `failed` immediately — even while attempts remain under the retry budget — with the reason `repeated identical gate failure (<tail>) — task design defect`, persist it, print the early stop, and SHALL NOT dispatch further attempts for that task. A differing fingerprint (a flake, or an agent whose new work moved the gate's failure) SHALL fall through to the normal retry decision unchanged. Gate-only reruns (re-runs of the same committed work) SHALL NOT be used as comparison targets, so an identical result from re-running a single branch never triggers the stop by itself. The persisted receipts are the only evidence used, so the detection survives crash and resume.

#### Scenario: identical gate failure stops before the retry budget

GIVEN a task whose attempt 1 (a full agent run) dies at the gate with `PARSE-FAIL nix/…`, whose attempt 3 (another full agent run, after the cheap gate rerun of attempt 2) dies with the same `PARSE-FAIL` tail and exit code
WHEN attempt 3 is reaped
THEN the task is marked `failed` with reason `repeated identical gate failure (…) — task design defect`, even though `max_attempts` has not been reached, and no attempt 4 is dispatched.

#### Scenario: a different fingerprint retries normally

GIVEN a task whose attempt 1 dies at the gate with reason A and attempt 2 (fresh agent) dies with reason B (the agent changed the files the gate checks)
WHEN attempt 2 is reaped
THEN the task stays in retry flow and attempt 3 may still be dispatched.

#### Scenario: a gate-only rerun never triggers alone

GIVEN a task whose attempt 1 failed at the gate and attempt 2 was only a cheap gate rerun of the same committed work (no agent), failing identically by construction
WHEN attempt 2 is reaped
THEN no early stop fires (the comparison target must be a full agent-driven attempt with new work).

#### Scenario: detection survives resume

GIVEN a task whose two agent-driven attempts both died at the gate with identical fingerprints, split across two `af run` invocations
WHEN the second run reaps the later attempt
THEN the receipts persisted by the first run still trigger the early stop.

### Requirement: Retry prompts reuse the gate-failure receipt

The "Previous attempts" block in a retry prompt SHALL quote the receipt's `error` verbatim. Because gate-failure receipts now carry the Failing tail of the gate output (see the state delta), a task retried after a gate failure SHALL present that tail — the last lines the gate printed — to the next agent, so the agent is told why the gate rejected its work rather than shown a banner or an empty line. This is the ADR-13 mechanism unchanged; it only consumes the improved receipt content.

#### Scenario: retry prompt shows the gate's failing line

GIVEN a task whose only failed attempt died at the gate with a trailing `YAML-FAIL k8s/openedusuite/deploy-drawio.yaml` line
WHEN attempt 2 renders its "Previous attempts" block
THEN the block contains that trailing line, exactly as recorded on the receipt.

#### Scenario: first attempt and non-gate failures are unchanged

GIVEN a task whose attempt 1 failed on a provider timeout (no gate)
WHEN attempt 2 renders
THEN the block quotes the timeout's first line as before, and a first attempt renders no block at all.
