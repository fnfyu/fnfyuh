# Security posture

## Enforced by the current completion slice

- Tool callers submit typed `ToolIntent` values; there is no arbitrary event-append
  RPC and no shell-string execution.
- Workspace paths are relative UTF-8 paths. Traversal, absolute/drive/UNC paths,
  detected symlinks, and missing parents are rejected conservatively.
- The trusted-host policy accepts one canonical root, disables host processes by
  default, and requires explicit approval for writes.
- Container policy uses a separate image-program namespace. It never reuses a host
  executable allowlist and requires an isolated backend capability set before process
  intents are accepted.
- Approved requests bind session, principal, backend, operation, exact intent, digest,
  nonce, and expiry. A nonce is consumed once per daemon process across broker instances.
- SQLite events are append-only through triggers, hash chained per branch, schema
  checked (schema 2 accepts legacy schema-1 rows for read-only replay), and verified
  against raw payload hashes.
- Durable terminal tool events contain bounded metadata and digests. Raw stdout/stderr
  are immutable artifacts referenced by `ArtifactCreated` events and checked against
  the owning session on retrieval.
- Subscription outbox rows are leased and persisted. ACK advances a cursor and NACK
  returns an in-flight message to the retry queue; delivery is at-least-once.
- `operation.inspect` records backend observations. Continuation inspects first,
  refuses running/unknown/not-tracked operations, and only retries a proven-not-found
  read-only operation. Unknown effects are never guessed successful.
- Model API keys and endpoints are composition-time configuration. They are not put in
  session events, prompts returned to clients, artifacts, or provider error messages.

## Explicit boundaries

`local-trusted-host` remains a convenience adapter, not a hostile-agent sandbox. It
does not provide handle-relative `openat2`/`CreateFileW` guarantees, process-tree
isolation, network isolation, secret mediation, or a VM boundary. Do not enable it for
untrusted repositories.

The Docker adapter is a real container execution path when its engine/image preflight
passes. The standard daemon image is deliberately socket-free; mounting a Docker
socket into the daemon grants host-admin-equivalent power and is only acceptable in a
separately documented development profile. `vm` mode maps to Docker Hyper-V isolation
and must be matched with a compatible Windows-container image/engine; it is not a
portable Firecracker implementation.

Network and secret capabilities remain denied. Container execution uses `--network
none`, dropped capabilities, no inherited environment, a read-only root, bounded
resources, and one workspace mount. Image provenance and a dedicated/rootless engine
remain deployment responsibilities.

## Operational recovery

A pending command receipt is a recovery lease. Clients must inspect the operation or
explicitly recover it before retrying. The outbox is at-least-once, so clients dedupe
by `event_id` and ACK only after applying the event. Artifact retrieval is session
scoped and should be treated as sensitive workspace output.
