# Capability: worktree

## MODIFIED Requirements

### Requirement: Merge serialization

Merges into the base branch SHALL be serialized per repository (one in-process lock) so concurrent task merges never race. Only gate-passing tasks SHALL merge. A merge conflict SHALL mark the task failed rather than force-pushing.

Before running `git merge`, af SHALL verify that the merge target (the repository's current checkout) is clean. If the target is dirty, af SHALL NOT run the merge, SHALL NOT modify the operator's working tree (no stash, no checkout, no abort — nothing ran), and SHALL return an error naming the first dirty path (`merge target is dirty (<path>); commit or stash before af merges`). The gate-passing attempt's work SHALL be preserved on its archived branch exactly as any other failed-to-merge attempt, so it can be recovered once the checkout is clean. Dirty means `git status --porcelain` is non-empty at the repository path; an unreadable status is treated as dirty (fail safe), never as clean.

#### Scenario: serialized merges

WHEN multiple tasks pass their gates at the same time
THEN merges happen one at a time and the base branch remains consistent.

#### Scenario: conflict fails task

WHEN a task branch conflicts with the base branch at merge time
THEN the task is marked `failed` (retry allowed), never force-pushed.

#### Scenario: dirty merge target never loses the attempt

GIVEN the repository checkout has an uncommitted edit to `notes.txt`
AND a task passes its gate
WHEN af attempts to merge the task branch
THEN `git merge` is not run, the operator's `notes.txt` edit is untouched, an error names `notes.txt`, and the task branch is archived (not deleted) so `af recover` can merge it once the checkout is clean.
