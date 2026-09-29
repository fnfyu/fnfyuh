# Tasks

## 1. Isolated backend

- **Files:** `crates/execution-broker/src/lib.rs`, `crates/policy-engine/src/lib.rs`, `apps/daemon/src/main.rs`, `.env.example`
- **Verify:** `cargo test --workspace`; capabilities reports preflight failure without host fallback
- **Done when:** container/VM selection is explicit, resource/network/root controls are visible, and unavailable engines fail closed.

## 2. Operation recovery and persistence

- **Files:** `crates/protocol/src/lib.rs`, `crates/session-engine/src/lib.rs`, `crates/agent-runtime/src/lib.rs`, daemon RPC handlers
- **Verify:** session-engine tests plus daemon inspect/continue/outbox/artifact integration
- **Done when:** inspect is durable, unsafe continuation is refused, outbox leases survive restart, and terminal artifacts commit atomically.

## 3. Provider

- **Files:** `crates/agent-runtime/src/lib.rs`, provider docs, `.env.example`
- **Verify:** Rust provider tests with a fake HTTP endpoint and `runtime.v1.models`
- **Done when:** exact endpoint configuration, bounded response parsing, request correlation, tool-call validation, and secret redaction are covered.

## 4. Clients

- **Files:** `sdk/src/*`, `apps/gateway/*`, `apps/web/*`, `apps/ide/*`, Docker/Compose
- **Verify:** `pnpm test`, `pnpm build:sdk`, `pnpm run web:smoke`, gateway HTTP/SSE smoke
- **Done when:** browser and IDE clients use only versioned RPC/events and reconnect via the durable outbox.

## 5. Close-out

- **Files:** `README.md`, `docs/security.md`, `docs/operations/*`, `docs/session/HANDOFF.md`
- **Verify:** `git diff --check`, fresh Node checks, WSL/Docker verification
- **Done when:** docs state actual boundaries and no claim exceeds fresh proof.
