import { mkdir, open, readFile, rename, unlink, writeFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import { randomUUID } from "node:crypto";
import { openaiCodexProvider } from "@earendil-works/pi-ai/providers/openai-codex";
import { createModels } from "@earendil-works/pi-ai";

const PROVIDER_ID = "openai-codex";
const credentialPath = process.env.HARNESS_CODEX_OAUTH_FILE
  ?? join(dirname(process.env.HARNESS_DB ?? ".runtime/events.sqlite"), "openai-codex-oauth.json");
const provider = openaiCodexProvider();
const authOperations = new Map();
let writeQueue = Promise.resolve();

async function readCredential() {
  try {
    const raw = await readFile(credentialPath, "utf8");
    const value = JSON.parse(raw);
    return value?.type === "oauth" && value.access && value.refresh ? value : undefined;
  } catch (error) {
    if (error?.code === "ENOENT") return undefined;
    throw error;
  }
}

async function persistCredential(value) {
  if (value === undefined) {
    try { await unlink(credentialPath); } catch (error) {
      if (error?.code !== "ENOENT") throw error;
    }
    return;
  }
  await mkdir(dirname(credentialPath), { recursive: true });
  const temp = `${credentialPath}.tmp.${process.pid}.${randomUUID()}`;
  await writeFile(temp, `${JSON.stringify(value)}\n`, { mode: 0o600 });
  await rename(temp, credentialPath);
}

async function withCredentialLock(work) {
  await mkdir(dirname(credentialPath), { recursive: true });
  const lockPath = `${credentialPath}.lock`;
  let handle;
  for (let attempt = 0; attempt < 200; attempt += 1) {
    try {
      handle = await open(lockPath, "wx");
      break;
    } catch (error) {
      if (error?.code !== "EEXIST") throw error;
      await new Promise((resolve) => setTimeout(resolve, 50));
    }
  }
  if (!handle) throw new Error("OpenAI Codex credential store is busy");
  try {
    return await work();
  } finally {
    await handle.close();
    await unlink(lockPath).catch(() => undefined);
  }
}

function summarizeCodexError(error) {
  const message = error instanceof Error ? error.message : String(error);
  if (/unsupported_country_region_territory/i.test(message)) {
    return "OpenAI 设备码登录受当前网络地区限制，请更换 OpenAI 支持的网络出口。";
  }
  if (/cloudflare|just a moment|cf-mitigated|status\s+403/i.test(message)) {
    return "OpenAI OAuth 被 Cloudflare 拦截（HTTP 403）。请用普通浏览器开启 JavaScript/Cookie，关闭代理或更换网络后重试；也可尝试设备码登录。";
  }
  if (/fetch failed/i.test(message)) {
    const causeCode = error?.cause?.code;
    return `OpenAI OAuth 网络请求失败${causeCode ? `（${causeCode}）` : ""}，请检查 7897 代理后重新登录。`;
  }
  if (message.length > 600) return `${message.slice(0, 600)}…`;
  return message;
}

function credentialStore() {
  return {
    async read(providerId) {
      return providerId === PROVIDER_ID ? readCredential() : undefined;
    },
    async list() {
      return (await readCredential()) ? [{ providerId: PROVIDER_ID, type: "oauth" }] : [];
    },
    async modify(providerId, mutate) {
      if (providerId !== PROVIDER_ID) return undefined;
      const operation = writeQueue.then(() => withCredentialLock(async () => {
        const next = await mutate(await readCredential());
        await persistCredential(next);
        return next;
      }));
      writeQueue = operation.then(() => undefined, () => undefined);
      return operation;
    },
    async delete(providerId) {
      if (providerId !== PROVIDER_ID) return;
      const operation = writeQueue.then(() => withCredentialLock(() => persistCredential(undefined)));
      writeQueue = operation.then(() => undefined, () => undefined);
      return operation;
    },
  };
}

const models = createModels({ credentials: credentialStore() });
models.setProvider(provider);

function textFromAssistant(message) {
  return (message?.content ?? [])
    .filter((block) => block.type === "text")
    .map((block) => block.text)
    .join("");
}

function toolCallsFromAssistant(message) {
  return (message?.content ?? [])
    .filter((block) => block.type === "toolCall")
    .map((block) => ({
      call_id: block.id,
      name: block.name,
      arguments: block.arguments,
    }));
}

function messageForPi(message, model, attachments, isLastUserMessage) {
  const timestamp = Date.now();
  if (message.role === "assistant") {
    return {
      role: "assistant",
      content: [{ type: "text", text: message.content }],
      api: "openai-codex-responses",
      provider: PROVIDER_ID,
      model,
      usage: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: 0, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } },
      stopReason: "stop",
      timestamp,
    };
  }
  const content = [{ type: "text", text: message.content }];
  if (isLastUserMessage) {
    for (const attachment of attachments ?? []) {
      if (attachment.media_type?.startsWith("image/") && attachment.content_base64) {
        content.push({ type: "image", data: attachment.content_base64, mimeType: attachment.media_type });
      }
    }
  }
  return { role: "user", content, timestamp };
}

export function modelOptionsFromParameters(parameters = {}) {
  const options = {};
  const reasoning = parameters?.reasoning_effort;
  const disableReasoning = reasoning === "off";
  if (typeof reasoning === "string" && reasoning && reasoning !== "auto") options.reasoning = reasoning;
  if (typeof parameters?.temperature === "number" && Number.isFinite(parameters.temperature)) {
    options.temperature = parameters.temperature;
  }
  const maxOutputTokens = Number.isInteger(parameters?.max_output_tokens) && parameters.max_output_tokens >= 16
    ? parameters.max_output_tokens
    : undefined;
  if (maxOutputTokens !== undefined) options.maxTokens = maxOutputTokens;
  if (disableReasoning || maxOutputTokens !== undefined) {
    options.onPayload = (payload) => ({
      ...(payload ?? {}),
      ...(disableReasoning ? { reasoning: { effort: "none", summary: "auto" } } : {}),
      ...(maxOutputTokens !== undefined ? { max_output_tokens: maxOutputTokens } : {}),
    });
  }
  return options;
}

function buildContext(input) {
  const messages = input.messages ?? [];
  const firstSystem = messages.find((message) => message.role === "system");
  const userMessages = messages.filter((message) => message.role !== "system");
  const lastUserIndex = userMessages.map((message) => message.role).lastIndexOf("user");
  const piMessages = userMessages.map((message, index) => messageForPi(
    message,
    input.model,
    input.attachments,
    message.role === "user" && index === lastUserIndex,
  ));
  const tools = (input.tools ?? []).flatMap((tool) => {
    try {
      return [{ name: tool.name, description: tool.description, parameters: JSON.parse(tool.input_schema) }];
    } catch {
      return [];
    }
  });
  return {
    ...(firstSystem ? { systemPrompt: firstSystem.content } : {}),
    messages: piMessages,
    ...(tools.length ? { tools } : {}),
  };
}

export async function completeCodex(input) {
  const model = models.getModel(PROVIDER_ID, input.model);
  if (!model) throw new Error(`Codex model is not installed in pi-ai catalog: ${input.model}`);
  const response = await models.completeSimple(model, buildContext(input), modelOptionsFromParameters(input.parameters));
  return {
    request_id: input.request_id,
    content: textFromAssistant(response),
    stop_reason: response.stopReason ?? "stop",
    tool_calls: toolCallsFromAssistant(response),
  };
}

async function waitForLoginNotice(operation) {
  const deadline = Date.now() + 1500;
  while (Date.now() < deadline && !operation.url && !operation.deviceCode && operation.status === "running") {
    await new Promise((resolve) => setTimeout(resolve, 25));
  }
}

function loginMethod(value) {
  return value === "device_code" ? "device_code" : "browser";
}

function operationResult(operation) {
  return {
    login_id: operation.id,
    method: operation.method,
    status: operation.status,
    url: operation.url,
    ...(operation.deviceCode ? { device_code: operation.deviceCode } : {}),
    ...(operation.error ? { error: operation.error } : {}),
  };
}

export async function startCodexLogin(params = {}) {
  const method = loginMethod(params?.method);
  const loginId = randomUUID();
  const controller = new AbortController();
  const operation = { id: loginId, method, status: "running", url: null, deviceCode: null, error: null, controller };
  authOperations.set(loginId, operation);
  operation.promise = models.login(PROVIDER_ID, "oauth", {
    signal: controller.signal,
    notify(event) {
      if (event.type === "auth_url") operation.url = event.url;
      if (event.type === "device_code") {
        operation.deviceCode = {
          verification_uri: event.verificationUri,
          user_code: event.userCode,
          interval_seconds: event.intervalSeconds,
          expires_in_seconds: event.expiresInSeconds,
        };
      }
    },
    async prompt(prompt) {
      if (prompt.type === "select") return method;
      if (prompt.type === "manual_code") {
        return new Promise((resolve) => {
          if (prompt.signal?.aborted) return resolve("");
          prompt.signal?.addEventListener("abort", () => resolve(""), { once: true });
        });
      }
      return "";
    },
  }).then(() => {
    operation.status = "authenticated";
  }).catch((error) => {
    operation.status = controller.signal.aborted ? "cancelled" : "error";
    operation.error = summarizeCodexError(error);
  });
  await waitForLoginNotice(operation);
  return operationResult(operation);
}

export async function codexAuthStatus(loginId = undefined) {
  const operation = loginId ? authOperations.get(loginId) : undefined;
  let authenticated = false;
  let expires = undefined;
  try {
    const auth = await models.getAuth(PROVIDER_ID);
    authenticated = Boolean(auth?.auth?.apiKey);
    const credential = await readCredential();
    expires = credential?.expires;
  } catch (error) {
    if (operation?.status === "running") operation.status = "error";
    if (operation && !operation.error) operation.error = summarizeCodexError(error);
  }
  return {
    authenticated,
    expires_at_ms: expires,
    ...(operation ? {
      method: operation.method,
      login_id: loginId,
      status: operation.status,
      url: operation.url,
      ...(operation.deviceCode ? { device_code: operation.deviceCode } : {}),
      error: operation.error,
    } : {}),
  };
}

export async function logoutCodex() {
  for (const operation of authOperations.values()) {
    if (operation.status === "running") operation.controller.abort();
  }
  await models.logout(PROVIDER_ID);
  return { logged_out: true };
}

export async function handleCodexRpc(payload) {
  const method = payload?.method;
  let result;
  if (method === "runtime.v1.codex.auth.start") result = await startCodexLogin(payload?.params);
  else if (method === "runtime.v1.codex.auth.status") result = await codexAuthStatus(payload?.params?.login_id);
  else if (method === "runtime.v1.codex.auth.logout") result = await logoutCodex();
  else return null;
  return { jsonrpc: "2.0", id: payload.id ?? null, result };
}
