# Design

- **Backend seam:** `ExecutionBroker` owns an `Arc<dyn ExecutionAdapter>`. Local and
  Docker adapters share approval/digest binding but not filesystem/process code.
- **Recovery:** operation projection stores intent, backend, and external handle. The
  adapter owns inspection; `ToolCoordinator` records observations and refuses unsafe
  retries. Terminal observations use the atomic session-engine finish path.
- **Persistence:** SQLite events remain the source of truth. Subscription outbox rows
  are populated by an insert trigger and branch-prefix backfill; artifact content is
  stored separately and referenced by immutable events.
- **Provider:** `ModelProvider` remains synchronous for this slice. The HTTP adapter
  maps only validated provider-neutral tool calls into typed intents; policy/broker is
  still the final authorization boundary.
- **Clients:** stdio remains the canonical daemon transport. `apps/gateway` translates
  HTTP POST/SSE to stdio and serves the static Web console; `apps/ide` reuses the SDK
  without importing Rust internals.

The standard runtime image does not carry a Docker socket. Container/VM deployments
must provide an engine-visible runner as an explicit deployment decision.
