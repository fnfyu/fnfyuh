import { createHash } from "node:crypto";
import { spawn } from "node:child_process";
import { createInterface } from "node:readline";

export const PROTOCOL_NAME = "local-first-harness";
export const PROTOCOL_VERSION = 1;
export const EVENT_SCHEMA_VERSION = 2;

export function canonicalize(value) {
  if (Array.isArray(value)) return value.map(canonicalize);
  if (value && typeof value === "object") {
    return Object.fromEntries(
      Object.keys(value)
        .sort()
        .map((key) => [key, canonicalize(value[key])]),
    );
  }
  return value;
}

export function canonicalJson(value) {
  return JSON.stringify(canonicalize(value));
}

export function sha256(value) {
  return `sha256:${createHash("sha256").update(value).digest("hex")}`;
}

export function requestDigest(value) {
  return sha256(canonicalJson(value));
}

function payloadOf(event) {
  return event?.payload ?? event;
}

function push(messages, dynamicTail, role, content) {
  const message = { role, content };
  messages.push(message);
  dynamicTail.push(message);
}

function stablePrefix(layers = {}, tools = []) {
  const toolSchema = [...tools]
    .sort((left, right) => String(left.name).localeCompare(String(right.name)))
    .map((tool) => `${tool.name}\n${tool.description ?? ""}\n${tool.input_schema ?? ""}`)
    .join("\n---\n");
  return [
    layers.system_rules ?? "",
    layers.tool_schema || toolSchema,
    layers.project_rules ?? "",
    (layers.frozen_summaries ?? []).join("\n"),
  ]
    .filter(Boolean)
    .join("\n\n");
}

function intentSummary(intent) {
  const kind = intent?.kind;
  const data = intent?.data ?? {};
  if (kind === "ReadFile") return `read ${data.path}`;
  if (kind === "Search") return `search ${data.root} for ${data.query}`;
  if (kind === "WriteFile") return `write ${data.path}`;
  if (kind === "EditFile") return `edit ${data.path}`;
  if (kind === "Exec" || kind === "Test") return `exec ${data.program} ${(data.args ?? []).join(" ")}`;
  if (kind === "Git") return `git ${(data.args ?? []).join(" ")}`;
  return "unknown tool intent";
}

/** Derive the same history shape regardless of event metadata or database IDs. */
export function stableModelHistory(events, layers = {}, tools = []) {
  const prefix = stablePrefix(layers, tools);
  const messages = [];
  const dynamicTail = [];
  if (prefix) messages.push({ role: "system", content: prefix });

  for (const event of events) {
    const payload = payloadOf(event);
    const type = payload?.type;
    const data = payload?.data ?? {};
    if (type === "UserMessage") push(messages, dynamicTail, "user", data.content ?? "");
    else if (type === "ModelResponded") push(messages, dynamicTail, "assistant", data.content ?? "");
    else if (type === "ToolProposed") {
      push(messages, dynamicTail, "assistant", `[tool proposed: ${data.tool_name}] ${intentSummary(data.intent)}`);
    } else if (type === "ToolFinished") {
      const result = data.result ?? {};
      push(
        messages,
        dynamicTail,
        "tool",
        `[tool result: ${result.status ?? "unknown"}]\nstdout_digest: ${result.stdout_digest ?? ""} (${result.stdout_bytes ?? 0} bytes)\nstderr_digest: ${result.stderr_digest ?? ""} (${result.stderr_bytes ?? 0} bytes)`,
      );
    } else if (type === "ToolFailed") {
      push(messages, dynamicTail, "tool", `[tool failure: ${data.error_code ?? "unknown"}] ${data.message ?? ""}`);
    } else if (type === "ContextCompacted") {
      const summary = { role: "summary", content: data.summary ?? "" };
      messages.splice(0, messages.length, ...messages.filter((message) => message.role === "system"));
      dynamicTail.splice(0, dynamicTail.length);
      messages.push(summary);
      dynamicTail.push(summary);
    }
  }

  const digest = requestDigest([prefix, messages, "prompt-compiler.v1"]);
  return {
    compiler_version: "prompt-compiler.v1",
    messages,
    stable_prefix: prefix,
    dynamic_tail: dynamicTail,
    digest,
  };
}

export class RpcError extends Error {
  constructor(code, message, data = undefined) {
    super(message);
    this.name = "RpcError";
    this.code = code;
    this.data = data;
  }
}

export class StdioJsonRpcTransport {
  #child;
  #pending = new Map();
  #listeners = new Set();
  #timeoutMs;
  #closed = false;

  constructor(command = "harnessd", args = [], options = {}) {
    this.#timeoutMs = options.timeoutMs ?? 120_000;
    this.#child = spawn(command, args, {
      cwd: options.cwd,
      env: options.env ?? process.env,
      stdio: ["pipe", "pipe", "inherit"],
    });
    const lines = createInterface({ input: this.#child.stdout });
    lines.on("line", (line) => {
      let message;
      try {
        message = JSON.parse(line);
      } catch {
        return;
      }
      if (message.id !== undefined && message.id !== null && this.#pending.has(message.id)) {
        const pending = this.#pending.get(message.id);
        this.#pending.delete(message.id);
        clearTimeout(pending.timeout);
        pending.resolve(message);
      } else {
        for (const listener of this.#listeners) listener(message);
      }
    });
    const rejectPending = (error) => {
      if (this.#closed) return;
      this.#closed = true;
      for (const pending of this.#pending.values()) {
        clearTimeout(pending.timeout);
        pending.reject(error);
      }
      this.#pending.clear();
    };
    this.#child.on("error", rejectPending);
    this.#child.on("exit", (code, signal) => {
      rejectPending(new Error(`stdio daemon exited${code === null ? "" : ` with code ${code}`}${signal ? ` (${signal})` : ""}`));
    });
    this.#child.on("close", (code, signal) => {
      rejectPending(new Error(`stdio daemon closed${code === null ? "" : ` with code ${code}`}${signal ? ` (${signal})` : ""}`));
    });
  }

  request(request) {
    if (this.#closed) return Promise.reject(new Error("stdio daemon is closed"));
    return new Promise((resolve, reject) => {
      const timeout = setTimeout(() => {
        if (!this.#pending.delete(request.id)) return;
        reject(new Error(`stdio request ${String(request.id)} timed out after ${this.#timeoutMs} ms`));
      }, this.#timeoutMs);
      this.#pending.set(request.id, { resolve, reject, timeout });
      this.#child.stdin.write(`${JSON.stringify(request)}\n`, (error) => {
        if (!error) return;
        const pending = this.#pending.get(request.id);
        if (!pending) return;
        this.#pending.delete(request.id);
        clearTimeout(pending.timeout);
        pending.reject(error);
      });
    });
  }

  onMessage(listener) {
    this.#listeners.add(listener);
    return () => this.#listeners.delete(listener);
  }

  close() {
    this.#child.kill();
  }
}

export class JsonRpcClient {
  #transport;
  #nextId = 1;
  #listeners = new Set();

  constructor(transport) {
    this.#transport = transport;
    if (typeof transport.onMessage === "function") {
      transport.onMessage((message) => this.#receive(message));
    }
  }

  async request(method, params = {}) {
    const id = `sdk-${this.#nextId++}`;
    const response = await this.#transport.request({ jsonrpc: "2.0", id, method, params });
    if (response?.error) {
      throw new RpcError(response.error.code, response.error.message, response.error.data);
    }
    return response?.result;
  }

  onEvent(listener) {
    this.#listeners.add(listener);
    return () => this.#listeners.delete(listener);
  }

  async subscribe(params = {}) {
    return this.request("runtime.v1.events.subscribe", params);
  }

  async inspectOperation(params) {
    return this.request("runtime.v1.session.operation.inspect", params);
  }

  async continueOperation(params) {
    return this.request("runtime.v1.session.operation.continue", params);
  }

  async executeTool(params) {
    return this.request("runtime.v1.tool.execute", params);
  }

  async pullEvents(params) {
    return this.request("runtime.v1.events.pull", params);
  }

  async ackEvents(params) {
    return this.request("runtime.v1.events.ack", params);
  }

  async nackEvent(params) {
    return this.request("runtime.v1.events.nack", params);
  }

  async getArtifact(params) {
    return this.request("runtime.v1.artifact.get", params);
  }

  async getSettings() {
    return this.request("runtime.v1.settings.get");
  }

  async saveSettings(settings) {
    return this.request("runtime.v1.settings.save", settings);
  }

  onSessionEvent(sessionId, listener) {
    return this.onEvent((event) => {
      if (!sessionId || event?.session_id === sessionId) listener(event);
    });
  }

  async subscribeSession(params = {}) {
    const result = await this.subscribe(params);
    const deliveries = result?.deliveries ?? [];
    return {
      ...result,
      deliveries,
      backlog: deliveries.length ? deliveries.map((delivery) => delivery.event) : (result?.events ?? []),
      pull: (options = {}) => this.pullEvents({
        subscription_id: result.subscription_id,
        limit: 100,
        lease_ms: 60_000,
        ...options,
      }),
      ack: (after_global_sequence) => this.ackEvents({
        subscription_id: result.subscription_id,
        after_global_sequence,
      }),
      nack: (message_id, error, retry_after_ms = 0) => this.nackEvent({
        subscription_id: result.subscription_id,
        message_id,
        error,
        retry_after_ms,
      }),
      onEvent: (listener) => this.onSessionEvent(params.session_id ?? result.session_id, listener),
    };
  }

  #receive(message) {
    if (message?.method === "session/event" || message?.method === "events.event") {
      for (const listener of this.#listeners) listener(message.params);
    }
  }
}

export function createInMemoryTransport(handler) {
  const listeners = new Set();
  return {
    request: async (request) => handler(request, (event) => {
      for (const listener of listeners) listener(event);
    }),
    onMessage(listener) {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
  };
}
