## ADDED / MODIFIED

### Requirement: Product identity and startup
#### Scenario: Start the local workbench
- **WHEN** a user runs `fnfyuh` or `pnpm fnfyuh`
- **THEN** the launcher starts the Web gateway with the product name `fnfyu harness`, without changing the compatibility protocol name `local-first-harness.v1`

### Requirement: Provider profiles and models
#### Scenario: Configure a provider without persisting a credential
- **WHEN** a user saves a provider profile with protocol, endpoint, model list, and secret reference
- **THEN** settings persist the profile and model metadata, return only whether the referenced secret is configured, and never return or event-log the secret value

#### Scenario: Select a model for a turn
- **WHEN** a turn names a configured model
- **THEN** the daemon resolves that model to its provider adapter and fails explicitly if the provider is disabled, missing, or malformed

### Requirement: Built-in workspace tools
#### Scenario: Read or inspect workspace content
- **WHEN** an agent proposes read_file, list_files, search, or read_image
- **THEN** the request is converted to a typed tool intent, evaluated by the existing workspace policy, executed by the selected backend, and recorded through the existing operation event chain

#### Scenario: Unsafe workspace request
- **WHEN** a tool path escapes the workspace, crosses a symlink/reparse point, exceeds the configured output limit, or requests a forbidden process
- **THEN** execution is rejected before side effects and the denial remains auditable

#### Scenario: Preview an image result
- **WHEN** read_image succeeds for a supported bounded image
- **THEN** the result is represented as a media artifact with type, digest, byte count, and retrievable content; the event log stores metadata rather than raw binary content

### Requirement: Existing client compatibility
#### Scenario: Existing session client connects
- **WHEN** an old client uses local-first-harness.v1 session and event methods
- **THEN** those methods retain their current request/response and event semantics while settings and new tools are additive
