# Web and IDE clients

## Gateway and Web console

The product-facing launcher is `fnfyuh`; `dsh` remains a compatibility alias. Start the
local gateway profile with:

```bash
HARNESS_WORKSPACE="$PWD" docker compose build harnessd
HARNESS_GATEWAY_TOKEN="local-dev-token" docker compose --profile web up gateway
```

Set `localStorage.setItem('harness.gateway.token', 'local-dev-token')` once in the
browser console. Open `http://127.0.0.1:8787`. The gateway spawns a stdio `harnessd`, forwards typed
JSON-RPC at `POST /rpc`, and exposes leased outbox deliveries as `GET /events` SSE.
For a non-loopback deployment set `HARNESS_GATEWAY_TOKEN`; place that value in
`localStorage` under `harness.gateway.token` before loading the Web client. The
browser never receives a provider key. The console supports session creation, model selection, provider/model settings,
turns, the built-in tool queue (read/list/search/image plus approved mutations), event
timeline, operation inspection, and explicit continuation.

The UI is a dependency-free static client using the persisted dark developer-tool
design system in `design-system/local-first-harness/MASTER.md`. It uses semantic
labels, visible keyboard focus, reduced-motion handling, responsive layouts, and
session-scoped event dedupe.

## IDE client

`apps/ide/client.mjs` is a small Node/IDE adapter over the same stdio SDK seam:

```bash
node apps/ide/client.mjs create .
node apps/ide/client.mjs turn <session-id> "inspect the workspace"
node apps/ide/client.mjs inspect <session-id> <operation-id>
```

An editor extension can import `HarnessIdeClient`, attach `onEvent`, and render
committed events without importing daemon internals. All mutations include a
`command_id` so editor retries receive durable receipts.

## Delivery contract

`runtime.v1.events.subscribe` creates a persistent subscription and returns leased
`deliveries`. Use `runtime.v1.events.pull` after reconnect, apply each event once by
`event_id`, then call `runtime.v1.events.ack` with the applied cursor. Call
`runtime.v1.events.nack` when a delivery cannot be applied. The contract is
at-least-once, not exactly-once.
