export class BrowserRpcError extends Error {
  constructor(code, message, data = undefined) {
    super(message);
    this.name = "BrowserRpcError";
    this.code = code;
    this.data = data;
  }
}

function safeStoredToken() {
  try {
    return globalThis.localStorage?.getItem("harness.gateway.token") ?? "";
  } catch {
    return "";
  }
}

export class FetchJsonRpcTransport {
  #baseUrl;
  #token;
  #nextId = 1;
  #listeners = new Set();

  constructor(baseUrl = "", options = {}) {
    this.#baseUrl = baseUrl.replace(/\/$/, "");
    this.#token = options.token ?? safeStoredToken();
  }

  async request(request) {
    const headers = { "content-type": "application/json" };
    if (this.#token) headers.authorization = `Bearer ${this.#token}`;
    const response = await fetch(`${this.#baseUrl}/rpc`, {
      method: "POST",
      headers,
      body: JSON.stringify(request),
    });
    if (!response.ok) {
      throw new Error(`gateway returned HTTP ${response.status}`);
    }
    return response.json();
  }

  nextRequest(method, params = {}) {
    return { jsonrpc: "2.0", id: `browser-${this.#nextId++}`, method, params };
  }

  get token() {
    return this.#token;
  }

  url(path) {
    if (!path.startsWith("/")) return `${this.#baseUrl}/${path}`;
    return `${this.#baseUrl}${path}`;
  }

  onMessage(listener) {
    this.#listeners.add(listener);
    return () => this.#listeners.delete(listener);
  }
}

export class BrowserHarnessClient {
  #transport;
  #seen = new Map();

  constructor(baseUrl = "", options = {}) {
    this.#transport = new FetchJsonRpcTransport(baseUrl, options);
  }

  async request(method, params = {}) {
    const response = await this.#transport.request(this.#transport.nextRequest(method, params));
    if (response?.error) {
      throw new BrowserRpcError(response.error.code, response.error.message, response.error.data);
    }
    return response?.result;
  }

  createSession(params) {
    return this.request("runtime.v1.session.create", params);
  }

  startTurn(params) {
    return this.request("runtime.v1.turn.start", params);
  }

  inspectOperation(params) {
    return this.request("runtime.v1.session.operation.inspect", params);
  }

  continueOperation(params) {
    return this.request("runtime.v1.session.operation.continue", params);
  }

  executeTool(params) {
    return this.request("runtime.v1.tool.execute", params);
  }

  pullEvents(params) {
    return this.request("runtime.v1.events.pull", params);
  }

  ackEvents(params) {
    return this.request("runtime.v1.events.ack", params);
  }

  nackEvent(params) {
    return this.request("runtime.v1.events.nack", params);
  }

  unsubscribe(params) {
    return this.request("runtime.v1.events.unsubscribe", params);
  }

  putArtifact(params) {
    return this.request("runtime.v1.artifact.put", params);
  }

  listArtifacts(params) {
    return this.request("runtime.v1.artifact.list", params);
  }

  resetSessionSeen(sessionId) {
    for (const [key, seenSessionId] of this.#seen) {
      if (seenSessionId === sessionId) this.#seen.delete(key);
    }
  }

  getArtifact(params) {
    return this.request("runtime.v1.artifact.get", params);
  }

  getSettings() {
    return this.request("runtime.v1.settings.get");
  }

  saveSettings(settings) {
    return this.request("runtime.v1.settings.save", settings);
  }

  listProviderModels(params) {
    return this.request("runtime.v1.provider.models.list", params);
  }

  startCodexLogin(method = "browser") {
    return this.request("runtime.v1.codex.auth.start", { method });
  }

  codexAuthStatus(loginId) {
    return this.request("runtime.v1.codex.auth.status", loginId ? { login_id: loginId } : {});
  }

  logoutCodex() {
    return this.request("runtime.v1.codex.auth.logout");
  }

  async subscribe(params = {}) {
    const result = await this.request("runtime.v1.events.subscribe", params);
    const deliveries = result?.deliveries ?? [];
    const events = result?.events ?? [];
    for (const delivery of deliveries) this.#remember(delivery?.event);
    for (const event of events) this.#remember(event);
    return {
      ...result,
      deliveries,
      events: events.length ? events : deliveries.map((delivery) => delivery.event),
    };
  }

  streamEvents({ sessionId, subscriptionId, afterGlobalSequence = 0, onEvent, onError } = {}) {
    const query = new URLSearchParams({
      session_id: sessionId ?? "",
      after_global_sequence: String(afterGlobalSequence),
      ...(subscriptionId ? { subscription_id: subscriptionId } : {}),
      ...(this.#transport.token ? { access_token: this.#transport.token } : {}),
    });
    const source = new EventSource(this.#transport.url(`/events?${query.toString()}`));
    source.addEventListener("session/event", (message) => {
      try {
        const event = JSON.parse(message.data);
        if (event.session_id === sessionId) {
          this.#remember(event);
          onEvent?.(event);
        }
      } catch (error) {
        onError?.(error);
      }
    });
    source.onerror = (error) => onError?.(error);
    return () => source.close();
  }

  #remember(event) {
    const key = event?.event_id ?? `${event?.session_id}:${event?.global_sequence}:${event?.sequence}`;
    if (!key || this.#seen.has(key)) return false;
    this.#seen.set(key, event?.session_id);
    if (this.#seen.size > 5000) this.#seen.delete(this.#seen.keys().next().value);
    return true;
  }
}
