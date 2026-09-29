# Model providers

The product is named `fnfyu harness`; the compatibility protocol and daemon still use
`local-first-harness.v1` and `harnessd`. A session stores the selected model name, prompt
digest, and normalized per-turn model parameters with each model request. Provider
profiles and model metadata are stored in the local
`fnfyuh-settings.json` settings file; credentials are referenced by environment-variable
name and resolved only inside the daemon provider adapter.

## Configured provider profiles

The Chinese Workbench settings can add a provider profile and one or more model ids.
The built-in profiles are OpenAI, DeepSeek, Ollama/local, Anthropic, and OpenAI Codex.
OpenAI, DeepSeek, and Ollama use the OpenAI-compatible adapter; Anthropic uses the
Messages adapter; OpenAI Codex uses the pi-ai OAuth bridge. A profile may use any
custom endpoint, so compatible gateways do not need a new code adapter.

## Per-turn model parameters

The Workbench exposes optional parameters under the composer and persists the preference
in the browser. They are sent as `runtime.v1.turn.start.params.parameters`:

- `reasoning_effort`: `auto`, `off`, `minimal`, `low`, `medium`, `high`, `xhigh`, or `max`;
- `temperature`: a number from `0` to `2`;
- `max_output_tokens`: an integer from `16` to `131072`.

Leaving a control at its default omits that field. The daemon validates the values and
records the validated parameters with `ModelRequested`. OpenAI-compatible and Anthropic
adapters map the values to their native request fields; the Codex bridge forwards the
thinking and sampling options to pi-ai and adds the Responses `max_output_tokens` field.
Providers may still reject or ignore a parameter they do not support. Anthropic accepts
temperature only from `0` to `1`, and extended thinking takes precedence over temperature.

API key fields are secret references such as `OPENAI_API_KEY`, not key values. The
settings response exposes only `api_key_configured: true|false`.

## OpenAI Codex OAuth

The Codex provider reuses `@earendil-works/pi-ai`'s `openaiCodexProvider()` and its
OAuth + PKCE flow. The gateway persists the opaque OAuth grant in the local runtime
data directory and pi-ai refreshes it before a request. It talks to the Codex backend,
not the OpenAI Platform endpoint, and never reads ChatGPT cookies or stores the grant
in the event log. The browser login callback uses port `1455` in the Docker Web profile.

## OpenAI-compatible HTTP

Any configured OpenAI-compatible model routes to `OpenAiCompatibleProvider`. Configure:

```text
HARNESS_MODEL_ENDPOINT=https://provider.example/v1/chat/completions
HARNESS_MODEL_API_KEY=<injected by the deployment secret store>
HARNESS_MODEL_TIMEOUT_MS=120000
```

The endpoint is exact; the provider does not guess a vendor URL or silently switch to another provider. Requests are non-streaming JSON with `messages`, `model`, `stream:false`, and
validated tool schemas. Responses are bounded, parsed into provider-neutral tool
calls, and normalized to the daemon's local request id before `ModelResponded` is
committed. Unknown or malformed model tool calls are rejected before proposal and
still pass through policy/broker authorization.

The API key is sent only as an Authorization header to the configured endpoint. It is
not persisted in events, receipts, artifacts, logs, browser payloads, or provider
error text. Billable network retries are intentionally not automatic; a timeout or
transport error leaves the session recovery-visible.

## Anthropic Messages

Anthropic profiles use the configured Messages endpoint, send `anthropic-version:
2023-06-01`, normalize text and `tool_use` blocks into the same provider-neutral
response, and never persist the `x-api-key` value. Tool calls still become typed
proposals and pass through the existing policy/broker seam.

## Health and verification

`runtime.v1.models` reports only whether endpoint/key configuration is present, never
the endpoint value. Use a fake local HTTP endpoint in tests. Live provider tests are
opt-in and must inject the key through the environment; do not put credentials in
`.env.example` or Git.
