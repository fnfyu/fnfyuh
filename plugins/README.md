# External plugins

Plugins are process-out-of-process adapters. The daemon validates a manifest before starting an adapter and communicates with it through the versioned protocol; no native plugin is imported into the daemon.

A manifest contains:

```json
{
  "name": "example-tool",
  "version": "0.1.0",
  "protocol_version": 1,
  "kind": "tool",
  "requested_capabilities": ["workspace_read"]
}
```

Capabilities are deny-by-default. A plugin may propose an execution intent, but it cannot call a shell, access the workspace, read secrets, or open a network connection without a broker-issued capability/approval. Container/WASI adapters are future implementations of this seam.
