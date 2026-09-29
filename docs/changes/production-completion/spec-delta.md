## ADDED / MODIFIED

### Requirement: Isolated execution backend
#### Scenario: Container backend is unavailable
- **WHEN** the configured engine or image preflight fails
- **THEN** capabilities report unavailable and a requested container/VM operation fails closed without running the host adapter

#### Scenario: Isolated tool execution
- **WHEN** an approved typed intent runs on the Docker backend
- **THEN** it uses a handle-relative `/workspace` mapping, no network, bounded resources, a read-only root, and no inherited secrets

### Requirement: Inspect-first long-operation recovery
#### Scenario: Backend operation is still running
- **WHEN** a client inspects or continues an unknown operation
- **THEN** the durable operation remains non-terminal and no duplicate start is issued

#### Scenario: Read-only backend operation is proven not found
- **WHEN** inspect returns `not_found` and the intent is read-only
- **THEN** explicit continuation may issue one new attempt and records continuation/start facts

### Requirement: Durable outbox and artifacts
#### Scenario: Client reconnects
- **WHEN** a subscription pulls after a disconnect
- **THEN** leased deliveries are replayed from SQLite, deduped by event id, and can be ACKed or NACKed

#### Scenario: Tool produces output
- **WHEN** a terminal tool result is committed
- **THEN** artifact rows, `ArtifactCreated`, `ToolFinished`, and optional `RunCompleted` facts commit together

### Requirement: Real provider and clients
#### Scenario: Configured model is selected
- **WHEN** a configured model is requested
- **THEN** the daemon calls its exact provider adapter and fails explicitly if the model is unavailable; it never silently switches providers

#### Scenario: Web or IDE reconnects
- **WHEN** a client uses the gateway/SDK
- **THEN** it consumes only versioned RPC/events and never receives provider credentials or daemon internals
