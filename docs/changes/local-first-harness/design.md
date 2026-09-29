# Design

## Module seams

- `protocol`: versioned wire types, event envelope, JSON-RPC request/response/event notifications. No filesystem, process, or database dependency.
- `session-engine`: append-only event store plus projection. The store is the only writer of durable facts; projection is rebuildable.
- `agent-runtime`: model/tool loop orchestration. It emits intents and consumes protocol-level results; it never shells out directly.
- `prompt-compiler`: deterministic projection-to-history compiler. Stable prefix, frozen summaries, dynamic tail are separate inputs.
- `policy-engine`: pure capability and approval decision module. It normalizes paths and command classes before a backend is called.
- `execution-broker`: one interface for local/container/remote adapters. Local restricted is the first adapter; container is a future adapter with the same intent/result shape.
- `plugin-host`: external process JSON-RPC adapter seam. The first slice defines manifest validation and does not load arbitrary in-process code.
- `daemon`: composition root and stdio JSON-RPC transport. It owns dependencies, not domain rules.
- `sdk`: TypeScript transport and typed client wrapper. It does not mirror private daemon state.

## Event invariants

1. Every event has protocol version, session id, monotonic sequence, event id, timestamp, and correlation ids where applicable.
2. Event payloads are immutable JSON values; append rejects a duplicate sequence or event id.
3. Replay is deterministic: projection and model-history derivation use canonical ordering and stable serialization.
4. Side effects are invisible until represented by events: proposal -> approval decision -> start -> terminal result.
5. A failed policy decision never reaches an execution adapter.

## Storage

SQLite is the first adapter. The event table is append-only and has a unique `(session_id, sequence)` key. Session metadata and projections are rebuildable indexes/snapshots, not a second source of truth. JSONL export is an explicit adapter for audit/replay.

Canonical JSON for protocol digests recursively sorts object keys in both Rust and TypeScript. The first slice avoids floating-point values in digest-bearing prompt/event structures; a full RFC 8785 implementation is a follow-up if providers require arbitrary numbers.

## Initial transport

The daemon uses newline-delimited JSON-RPC 2.0 over stdio for local CLI/CI and a stream of JSON-RPC notifications for events. TCP/WebSocket transport is deferred until the protocol has compatibility tests.

## Security boundary

The policy engine receives a workspace root set and binds every filesystem target to a relative workspace path. The first local adapter currently accepts exactly one canonical workspace root (multi-root binding is reserved for a later protocol revision), rejects traversal, absolute/drive/UNC-style paths, detected symlinks, and shell metacharacters, and never uses a shell. Host process execution is deny-by-default and remains disabled unless an explicit absolute-executable allowlist and trusted-workspace mode are configured. Network and secret capabilities are not mediated by this host mode.

This local adapter is intentionally **not** a hostile-agent sandbox: preflight path checks cannot eliminate every symlink/junction TOCTOU race on every OS, direct host processes are not a complete process-tree isolation boundary, and an approved host process may reach ambient network/secret resources. The implementation must not claim otherwise. Approval digests bind session, principal, backend, operation and exact intent, require a non-empty nonce and expiry, and are consumed once in the broker. A container/VM adapter is the required seam for untrusted repositories; it must report handle-relative/no-follow filesystem and process/network isolation capabilities before PolicyEngine can grant those capabilities. If an adapter cannot prove a capability, it fails closed.

## Verification strategy

- Rust unit/integration tests for protocol, event store, projection, policy and local execution adapters once Cargo is available.
- TypeScript contract tests for canonical event derivation and JSON-RPC client behavior in the current Node environment.
- Static source checks and documented toolchain blocker until `cargo test --workspace` is available.
