# Proposal

- **Status:** implemented and verified in the WSL/Docker builder
- **Why:** close the remaining runtime seams without changing the event-sourced client contract.
- **What changes:** add an adapter-owned isolated execution path, inspect-first operation
  recovery, durable leased outbox and artifact store, a real env-configured model
  provider, and Web/IDE clients over the versioned JSON-RPC seam.
- **Safety boundary:** trusted-host remains opt-in convenience mode; container/Hyper-V
  modes fail closed when their engine/image preflight is unavailable. Provider secrets
  remain composition-only.
- **Rollback:** select a previously configured provider/model, or remove the new
  client/gateway entrypoints. The append-only event schema accepts legacy schema-1 rows
  for replay.
