# Tasks

## 1. Establish the project and protocol foundation

- **Files:** `Cargo.toml`, `crates/protocol/**`, `sdk/**`, `package.json`, `tsconfig.json`
- **Verify:** `pnpm test` and WSL Docker builder `cargo test --workspace` → protocol/SDK/Rust tests pass
- **Done when:** Rust and TypeScript expose one versioned event/RPC vocabulary without leaking implementation types
- **Status:** done (WSL Docker builder verified)

## 2. Implement durable session history

- **Files:** `crates/session-engine/**`, SQLite migrations, projection/replay tests
- **Verify:** `cargo test -p harness-session-engine` → append/replay/resume/fork cases pass
- **Done when:** one event log is sufficient to rebuild session state and deterministic model history
- **Status:** done (WSL Docker builder verified)

## 3. Implement policy and execution seams

- **Files:** `crates/policy-engine/**`, `crates/execution-broker/**`, tool intent types, security tests
- **Verify:** `cargo test -p harness-policy-engine -p harness-execution-broker` → traversal, command, approval, and correlation cases pass
- **Done when:** file and shell capabilities share policy roots and denied intents never start
- **Status:** done (WSL Docker builder verified)

## 4. Compose the daemon and client

- **Files:** `apps/daemon/**`, `apps/cli/**`, `sdk/**`
- **Verify:** `pnpm test`, WSL Docker health/create/turn/replay/command-receipt/subscription-backlog smoke → typed daemon and committed events pass
- **Done when:** a disconnected client can reconnect through the stable protocol and receive ordered events
- **Status:** done (WSL Docker smoke verified)

## 5. Review and close the first vertical slice

- **Files:** docs, security notes, `docs/session/HANDOFF.md`
- **Verify:** fresh Node checks, WSL Docker builder tests, `git diff --check`, focused Spec/Standards review
- **Done when:** acceptance gaps and Rust verification blocker are explicit; no unsupported production-security claim is made
- **Status:** doing (focused review completed; formal two-axis review awaits a committed fixed point)
