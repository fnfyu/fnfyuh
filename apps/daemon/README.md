# `harnessd`

`harnessd` speaks newline-delimited JSON-RPC over stdin/stdout. The optional
`apps/gateway` process exposes the same protocol to browser clients over HTTP + SSE.

```text
HARNESS_DB=.runtime/events.sqlite cargo run -p harness-daemon
```

## JSON-RPC surface

- `runtime.v1.health`
- `runtime.v1.models`
- `runtime.v1.execution.capabilities`
- `runtime.v1.session.create`
- `runtime.v1.session.events`
- `runtime.v1.session.replay`
- `runtime.v1.session.resume`
- `runtime.v1.session.operation.inspect` (durable projection plus backend observation)
- `runtime.v1.session.operation.continue` (explicit, inspect-first continuation; read-only retries only)
- `runtime.v1.session.recover` (records explicit recovery and never guesses external success)
- `runtime.v1.tool.execute` (typed tool coordinator with backend-bound audit events)
- `runtime.v1.session.fork`
- `runtime.v1.turn.start` (configured provider, including OpenAI Codex OAuth; optional image attachments for vision models)
- `runtime.v1.session.prompt`
- `runtime.v1.events.replay`
- `runtime.v1.events.subscribe`
- `runtime.v1.events.pull`
- `runtime.v1.events.ack`
- `runtime.v1.events.nack`
- `runtime.v1.events.unsubscribe`
- `runtime.v1.artifact.put` (session-scoped upload; does not write to the workspace)
- `runtime.v1.artifact.list`
- `runtime.v1.artifact.get`

Supplying a stable `command_id` enables the SQLite receipt ledger. Committed retries
return the original result with `receipt.replayed=true`; pending commands refuse a
second side effect until explicit recovery. Session events are hash chained and
outbox deliveries are leased, acknowledged, and retryable after a process restart.
Artifact content is stored separately from the event log and is reachable only with
the owning session id.

## Backend selection

- `HARNESS_EXECUTION_BACKEND=local-trusted-host` is the default convenience mode and
  is **not** a hostile-agent sandbox.
- `HARNESS_EXECUTION_BACKEND=container` uses `DockerContainerBackend` with a private
  network, dropped capabilities, read-only root, bounded resources, an isolated
  `/workspace` mount, and no inherited environment. The runtime and image must pass
  preflight; unavailable Docker fails closed.
- `HARNESS_EXECUTION_BACKEND=vm` requests Docker's explicit Hyper-V isolation flag.
  Use only with a Windows-container image and a Docker engine that supports it; this
  setting is not a promise that a Linux container is a VM.

Use an immutable `HARNESS_CONTAINER_IMAGE` digest in deployments. The standard
runtime image intentionally does not include a Docker socket; use a dedicated/rootless
runner or a clearly marked development profile if the daemon needs an engine.

The HTTP providers read `HARNESS_MODEL_ENDPOINT`, optional `HARNESS_MODEL_API_KEY`, and
`HARNESS_MODEL_TIMEOUT_MS`. The optional `OpenAI Codex` provider delegates to the local
Node bridge, which reuses `@earendil-works/pi-ai`'s `openaiCodexProvider().auth.oauth`
flow and `openai-codex` transport. It performs OAuth + PKCE, persists the opaque grant,
and refreshes it before requests; it never reads ChatGPT cookies or session tokens.
Use `HARNESS_CODEX_BRIDGE`, `HARNESS_CODEX_OAUTH_FILE`, `HARNESS_CODEX_NODE`, and
`HARNESS_CODEX_TIMEOUT_MS`. OAuth callback binding is controlled by
`PI_OAUTH_CALLBACK_HOST`; image attachments are projected by pi-ai when the selected
Codex model is marked vision-capable. Keys and grants are never written to events or
sent to browser/IDE clients. See `docs/providers.md` and `docs/clients.md`.
