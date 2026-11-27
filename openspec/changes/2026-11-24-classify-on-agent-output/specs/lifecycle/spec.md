# Capability: lifecycle

## ADDED Requirements

### Requirement: Dispatch waits for a clean merge target

`af` SHALL NOT dispatch an attempt for a task whose merge target (the repository's current checkout) is dirty, because a gate-passing attempt could not be merged into it and a 30-minute agent run would be lost. A dirty target parks the dispatch: the task stays pending, NO receipt is written, NO attempt of `max_attempts` is consumed, and the loop re-checks the target on every cycle so dispatch resumes by itself the moment the operator commits or stashes. The loop SHALL log the reason (naming the first dirty path) once per distinct dirty set rather than on every cycle. An UNREADABLE target status (the path is not a git repository or does not exist) SHALL NOT park — that is the attempt's own error to report through the normal worktree failure path; parking on it would leave a task that can never dispatch. This check is the pre-flight; the merge-time guard in `worktree` remains as the backstop for a target dirtied mid-attempt.

#### Scenario: dirty checkout parks dispatch without spending an attempt

GIVEN a pending task with `max_attempts: 3`
AND the repository checkout has an uncommitted edit
WHEN `af run` performs a dispatch cycle
THEN no agent is spawned, the task's attempt count is unchanged, no receipt is written, and the log names the dirty path.

#### Scenario: dispatch resumes when the checkout is clean

GIVEN a task parked by a dirty checkout
WHEN the operator commits the edit
THEN the next dispatch cycle runs the attempt normally.

#### Scenario: a clean checkout dispatches immediately

GIVEN a pending task and a clean checkout
WHEN `af run` performs a dispatch cycle
THEN the attempt is dispatched with no parking and no extra log output.

#### Scenario: an unreadable target is the attempt's error, not a park

GIVEN a pending task whose repository path exists but is not a git repository
WHEN `af run` performs a dispatch cycle
THEN the attempt is dispatched (not parked) and fails cleanly through the worktree error path, so the task reaches `failed` instead of spinning forever.
