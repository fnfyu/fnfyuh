## ADDED / MODIFIED

### Requirement: Event-sourced sessions
#### Scenario: Derive model context from durable events
- **WHEN** a session has immutable turn/step/model/tool events
- **THEN** the prompt history is derived deterministically from the event log and no UI mutation is authoritative

### Requirement: Resumable and forkable sessions
#### Scenario: Resume after client disconnect
- **WHEN** a client reconnects with a session id
- **THEN** the daemon loads the event projection and continues from the last durable sequence

#### Scenario: Fork at a known sequence
- **WHEN** a client requests a fork at an existing sequence
- **THEN** a new session copies the prefix as provenance and appends future events independently

### Requirement: Capability-gated execution
#### Scenario: Tool execution outside the workspace
- **WHEN** a read/write/process intent targets a path or command outside its granted policy
- **THEN** the policy engine rejects it before the execution backend starts and records the denial

#### Scenario: Approved side effect
- **WHEN** an intent requires approval and approval is granted
- **THEN** approval, start, and terminal result events are durable and correlated by operation id

### Requirement: Stable client seam
#### Scenario: Client consumes daemon events
- **WHEN** a JSON-RPC client subscribes to a session
- **THEN** it receives versioned responses and ordered stream events without importing daemon internals
