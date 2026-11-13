# Campaign lifecycle

`af run` executes a **campaign**: the declarative inputs (`tasks.json`,
`workers.json`, `AF_*` environment) feed the orchestrator, which prepares a
fresh git worktree + branch per ready task, dispatches its prompt to an agent
CLI worker chosen by UCB1 trust routing, and treats the task's `accept`
command as the sole arbiter of success — exit `0` merges the branch, any
other ending (gate failure, stall, timeout, scope violation, conflict)
archives it as `<branch>-rejected-<ts>`. Every ending writes a receipt that
feeds the trust + cost ledger, which in turn routes the next dispatch; failed
attempts retry up to `max_attempts` (the retry prompt lists earlier failures),
and archived branches that already satisfy the task's current scope are
re-validated and merged without ever invoking an agent again.

```mermaid
flowchart LR
    subgraph inputs[Inputs]
        T["tasks.json<br/>N declarative tasks"]
        W["workers.json<br/>M providers"]
        E["env (AF_*)"]
    end

    T --> AF
    W --> AF
    E --> AF

    AF["af run"] --> REUSE{"archived branch<br/>satisfies current scope?"}
    REUSE -->|yes| RGATE["re-run accept gate<br/>on archived branch"]
    RGATE -->|exit 0| M
    REUSE -->|"no · AF_NO_REUSE=1"| WT["Prepare<br/>fresh worktree + branch"]
    WT --> A["Dispatch<br/>agent CLI · UCB1 routing"]
    A --> G{"Verify<br/>accept gate"}
    G -->|exit 0| M["Merge<br/>to base branch"]
    G -->|"fail · stall · timeout<br/>scope · conflict"| AR["Archive<br/>branch-rejected-ts"]
    AR -->|"attempts < max_attempts<br/>retry prompt lists earlier failures"| WT
    M --> RC["receipt<br/>outcome · wall-clock · cost"]
    AR -->|"attempts exhausted"| RC
    RGATE -->|"gate fail<br/>kept, agent dispatched"| WT
    RC -->|Learn| UCB["trust + cost ledger<br/>→ next dispatch"]
    UCB -.->|routes| A
```
