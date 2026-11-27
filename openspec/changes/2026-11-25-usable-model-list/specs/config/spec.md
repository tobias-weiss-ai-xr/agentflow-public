# Capability: config

## ADDED Requirements

### Requirement: Worker validation warns on excluded model ids

`af validate` (and config loading) SHALL produce a WARNING — never a load
error — when an ENABLED worker's `model` string matches a maintained
exclusion list of model ids with a hard, multi-campaign failure record or an
operator directive (`KNOWN_PROBLEM_MODELS`; the evidence ledger and the
maintainer rule live in `docs/workers.md`). A disabled worker SHALL NOT warn,
as it is never dispatched. Matching SHALL be case-insensitive substring over
the declared model string (e.g. `tud/kimi-k2.6` and `tud/Kimi-K2.6` both
match `kimi`), so prefixed route forms are caught while the warning stays
advisory: agentflow never interprets or whitelists worker CLI arguments
(ADR-1 CLI-agnostic pass-through), and a host with a deliberate, working use
of such a model must not be blocked. The warning SHALL name the worker, the
model, and point at `docs/workers.md`. A model whose failure record is a
concurrency problem rather than model quality (e.g. `glm-5.2-awq`: backend
self-saturation under parallel load) SHALL NOT be on the exclusion list — the
fix is the `max_in_flight` default, not exclusion.

#### Scenario: enabled worker on an excluded model warns

GIVEN an enabled worker whose `model` is `tud/kimi-k2.6`
WHEN the workers config is loaded
THEN loading succeeds with a warning naming that worker and its model.

#### Scenario: prefixed route and mixed case also match

GIVEN an enabled worker whose `model` is `tud/Kimi-K2.6`
WHEN the workers config is loaded
THEN the same warning fires (case-insensitive substring match).

#### Scenario: disabled worker on an excluded model does not warn

GIVEN a disabled worker (`enabled: false`) whose `model` is `tud/kimi-k2.6`
WHEN the workers config is loaded
THEN no warning about that worker is produced, mirroring the cost-basis rule.

#### Scenario: a saturation-prone model is not excluded

GIVEN an enabled worker whose `model` is `tud/glm-5.2-awq` with
`max_in_flight: 1`
WHEN the workers config is loaded
THEN no exclusion warning is produced — its failure mode is concurrency and
the caller has applied the serialized default.

### Requirement: Per-worker in-flight concurrency

Each worker SHALL support `max_in_flight` — the maximum concurrent attempts
dispatched on that worker at once. `0` (the default when absent, and the
legacy behavior) SHALL mean one in-flight attempt per worker, keeping a
single worker entry from ever being double-dispatched. A value `N > 0` SHALL
allow up to `N` concurrent attempts on that worker name, so one worker entry
expresses an `N`-instance model pool without duplicate entries. The dispatch
loop SHALL respect the cap by counting in-flight attempts per worker name and
excluding a worker once its count reaches its cap.

#### Scenario: the default is one in-flight attempt per worker

GIVEN a worker with no `max_in_flight` field
WHEN attempts are dispatched
THEN at most one attempt runs on that worker at a time.

#### Scenario: an explicit instance count widens the pool

GIVEN a worker with `max_in_flight: 2`
WHEN attempts are dispatched
THEN up to two attempts run concurrently on that worker.

#### Scenario: the fleet defaults encode the campaign concurrency lessons

GIVEN the default fleet (`write_workers()` in the fleet generation script
`write_workers()`)
THEN `tud/mistral-medium-3.5-128b` has `max_in_flight: 2` (strongest verified
tool-caller), `tud/glm-5.2-awq` has `max_in_flight: 1` (serialized — its
chronic stalls are backend self-saturation under parallel load), and
`zai/glm-5.3-flash` / `local/deepseek-v4-flash/ai1` each have
`max_in_flight: 1`.
