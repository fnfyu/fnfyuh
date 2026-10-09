import { readFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import { openaiCodexProvider } from "@earendil-works/pi-ai/providers/openai-codex";

const defaultSettingsPath = process.env.HARNESS_SETTINGS
  ?? join(dirname(process.env.HARNESS_DB ?? ".runtime/events.sqlite"), "fnfyuh-settings.json");

async function readSettingsProvider(providerId, settingsPath = defaultSettingsPath) {
  try {
    const raw = JSON.parse(await readFile(settingsPath, "utf8"));
    return (raw.providers ?? []).find((provider) => provider.id === providerId);
  } catch (error) {
    if (error?.code === "ENOENT") return undefined;
    throw error;
  }
}

function modelsFromCatalog(models) {
  return models.map((model) => ({
    id: model.id,
    label: model.name ?? model.id,
    supports_vision: Boolean(model.input?.includes?.("image") || model.supportsVision),
  }));
}

function modelListUrl(endpoint) {
  const value = String(endpoint ?? "").replace(/\/+$/, "");
  if (!value) throw new Error("provider endpoint is required");
  const parsed = new URL(value);
  if (parsed.protocol !== "http:" && parsed.protocol !== "https:") {
    throw new Error("provider endpoint must use http or https");
  }
  const normalized = parsed.href.replace(/\/+$/, "")
    .replace(/\/chat\/completions$/i, "/models")
    .replace(/\/messages$/i, "/models")
    .replace(/\/responses$/i, "/models");
  return /\/models$/i.test(normalized) ? normalized : `${normalized}/models`;
}

function providerFromInput(input, stored) {
  if (stored) return stored;
  return {
    ...(input?.kind ? { kind: input.kind } : {}),
    ...(input?.endpoint ? { endpoint: input.endpoint } : {}),
  };
}

async function listRemoteModels(provider) {
  const url = modelListUrl(provider.endpoint);
  const headers = { accept: "application/json" };
  const secret = provider.api_key_env ? process.env[provider.api_key_env] : undefined;
  if (provider.kind === "anthropic") {
    headers["anthropic-version"] = "2023-06-01";
    if (secret) headers["x-api-key"] = secret;
  } else if (secret) {
    headers.authorization = `Bearer ${secret}`;
  }
  const response = await fetch(url, { headers });
  const text = await response.text();
  if (!response.ok) throw new Error(`model listing request failed (${response.status})`);
  let body;
  try {
    body = JSON.parse(text);
  } catch {
    throw new Error("model listing response was not JSON");
  }
  const entries = Array.isArray(body?.data)
    ? body.data
    : body?.models && typeof body.models === "object"
      ? Object.entries(body.models).map(([id, value]) => ({ id, ...(value ?? {}) }))
      : [];
  return entries
    .map((model) => ({
      id: typeof model?.id === "string" ? model.id : "",
      label: typeof model?.name === "string" ? model.name : typeof model?.id === "string" ? model.id : "",
      supports_vision: Boolean(model?.supports_vision || model?.supportsVision || model?.capabilities?.vision),
    }))
    .filter((model) => model.id);
}

export async function listProviderModels(input = {}, options = {}) {
  const providerId = String(input.provider_id ?? "").trim();
  if (!providerId) throw new Error("provider_id is required");
  const stored = await readSettingsProvider(providerId, options.settingsPath);
  const provider = providerFromInput(input, stored);
  if (provider.kind === "openai_codex") {
    return { provider_id: providerId, models: modelsFromCatalog(openaiCodexProvider().getModels()) };
  }
  return { provider_id: providerId, models: await listRemoteModels(provider) };
}

export async function handleProviderModelsRpc(payload) {
  if (payload?.method !== "runtime.v1.provider.models.list") return null;
  const result = await listProviderModels(payload.params);
  return { jsonrpc: "2.0", id: payload.id ?? null, result };
}
