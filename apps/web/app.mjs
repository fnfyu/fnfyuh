import { BrowserHarnessClient } from "/sdk/browser.mjs";

const client = new BrowserHarnessClient("");
const ARCHIVED_SESSIONS_KEY = "fnfyuh.archived-sessions";
const HIDDEN_SESSIONS_KEY = "fnfyuh.hidden-sessions";
const LOCAL_ARTIFACTS_PREFIX = "fnfyuh.session-artifacts.";
const LOCAL_HIDDEN_ARTIFACTS_PREFIX = "fnfyuh.hidden-artifacts.";
const LOCAL_SESSION_CATALOG_KEY = "fnfyuh.session-catalog";
const TURN_PARAMETERS_KEY = "fnfyuh.turn-parameters";
const MAX_ATTACHMENT_BYTES = 750_000;
const MAX_TEXT_ATTACHMENT_CHARS = 240_000;
const NO_MODEL_LABEL = "未选择模型";
const MAX_HISTORY_PAGES = 20;

const state = {
  sessionId: null,
  sessionGeneration: 0,
  subscriptionId: null,
  closeStream: null,
  events: [],
  lastOperationId: null,
  settings: null,
  tools: new Map(),
  sessions: loadLocalSessionCatalog(),
  sessionFilter: "active",
  sessionSearch: "",
  archivedSessionIds: loadStringSet(ARCHIVED_SESSIONS_KEY),
  hiddenSessionIds: loadStringSet(HIDDEN_SESSIONS_KEY),
  attachments: [],
  hiddenArtifactIds: new Set(),
  queuedAttachmentIds: new Set(),
  messageAttachments: new Map(),
  pendingUserMessage: null,
  pendingAssistantMessage: null,
  uploadingCount: 0,
  turnInFlight: false,
  historyLoading: false,
  historyLoadGeneration: 0,
  settingsLoadGeneration: 0,
  turnParameters: loadTurnParameters(),
};

function loadLocalSessionCatalog() {
  try {
    const value = JSON.parse(window.localStorage?.getItem(LOCAL_SESSION_CATALOG_KEY) ?? "[]");
    return Array.isArray(value) ? value.filter((item) => item?.id) : [];
  } catch {
    return [];
  }
}

function persistSessionCatalog() {
  try {
    const compact = state.sessions.slice(0, 200).map((session) => ({
      ...session,
      lastContent: String(session.lastContent ?? "").slice(0, 160),
    }));
    window.localStorage?.setItem(LOCAL_SESSION_CATALOG_KEY, JSON.stringify(compact));
  } catch {
    // The durable event log is the source of truth when local storage is unavailable.
  }
}

const THINKING_EFFORT_LABELS = {
  off: "关闭",
  minimal: "极低",
  low: "低",
  medium: "中",
  high: "高",
  xhigh: "极高",
  max: "最大",
};

function normalizeTurnParameters(value = {}) {
  const reasoning = ["off", "minimal", "low", "medium", "high", "xhigh", "max"].includes(value?.reasoning_effort)
    ? value.reasoning_effort
    : null;
  const rawTemperature = value?.temperature;
  const temperature = rawTemperature === "" || rawTemperature === null || rawTemperature === undefined
    ? null
    : Number(rawTemperature);
  const rawMaxTokens = value?.max_output_tokens;
  const maxOutputTokens = rawMaxTokens === "" || rawMaxTokens === null || rawMaxTokens === undefined
    ? null
    : Number(rawMaxTokens);
  return {
    reasoning_effort: reasoning,
    temperature: Number.isFinite(temperature) && temperature >= 0 && temperature <= 2
      ? Math.round(temperature * 10) / 10
      : null,
    max_output_tokens: Number.isInteger(maxOutputTokens) && maxOutputTokens >= 16 && maxOutputTokens <= 131072
      ? maxOutputTokens
      : null,
  };
}

function loadTurnParameters() {
  try {
    return normalizeTurnParameters(JSON.parse(window.localStorage?.getItem(TURN_PARAMETERS_KEY) ?? "{}"));
  } catch {
    return normalizeTurnParameters();
  }
}

function persistTurnParameters(parameters) {
  try {
    window.localStorage?.setItem(TURN_PARAMETERS_KEY, JSON.stringify(parameters));
  } catch {
    // Turn preferences are a convenience; an unavailable localStorage must not block sending.
  }
}

const EVENT_TYPE_LABELS = {
  session_created: "创建会话",
  session_forked: "会话分支",
  turn_started: "回合开始",
  user_message: "用户消息",
  run_started: "开始运行",
  model_requested: "模型请求",
  model_responded: "模型响应",
  tool_proposed: "工具请求",
  tool_approval_requested: "待审批",
  approval_requested: "待审批",
  tool_started: "工具开始",
  tool_finished: "工具完成",
  tool_failed: "工具失败",
  operation_started: "操作开始",
  operation_finished: "操作完成",
  operation_failed: "操作失败",
  recovery_requested: "请求恢复",
  recovery_required: "需恢复",
  recovery_committed: "恢复完成",
  artifact_created: "附件创建",
  session_completed: "会话完成",
};

const OPERATION_STATUS_LABELS = {
  approval_requested: "待审批",
  recovery_required: "需恢复",
  proposed: "已提议",
  approved: "已批准",
  execution_requested: "请求执行",
  pending: "待处理",
  unknown: "未知",
  started: "执行中",
  running: "执行中",
  succeeded: "成功",
  failed: "失败",
  denied: "已拒绝",
  cancelled: "已取消",
  canceled: "已取消",
  completed: "完成",
};

const RUNTIME_ERROR_LABELS = {
  "session not found": "未找到会话",
  "operation not found": "未找到操作",
  "gateway authentication required": "需要身份验证",
  "request body is too large": "内容过大",
  "connection refused": "无法连接",
  "failed to fetch": "无法连接",
  "fetch failed": "无法连接",
  timeout: "请求超时",
  "timed out": "请求超时",
  invalid: "参数无效",
};

const STOP_REASON_LABELS = {
  completed: "完成",
  stop: "已停止",
  length: "达到长度限制",
  tool_calls: "待工具调用",
};

const $ = (selector) => document.querySelector(selector);
const createForm = $("#create-form");
const inspectForm = $("#inspect-form");
const turnForm = $("#turn-form");
const turnContent = $("#turn-content");
const sendButton = $("#send-button");
const continueButton = $("#continue-button");
const toolQueue = $("#tool-queue");
const toolCount = $("#tool-count");
const modelSelect = $("#model-name");
const toast = $("#toast");
const settingsDialog = $("#settings-dialog");
const settingsForm = $("#settings-form");
const settingsStatus = $("#settings-status");
const providerList = $("#provider-list");
const providerCount = $("#provider-count");
const defaultModelSelect = $("#default-model");
const thinkingEffortSelect = $("#thinking-effort");
const temperatureInput = $("#temperature");
const maxOutputTokensInput = $("#max-output-tokens");
const turnParametersSummary = $("#turn-options-summary");
const sessionDialog = $("#session-dialog");
const saveDialog = $("#workspace-save-dialog");
const saveForm = $("#workspace-save-form");
const previewDialog = $("#media-preview-dialog");

function loadStringSet(key) {
  try {
    const value = JSON.parse(window.localStorage?.getItem(key) ?? "[]");
    return new Set(Array.isArray(value) ? value.filter((item) => typeof item === "string") : []);
  } catch {
    return new Set();
  }
}

function persistStringSet(key, values) {
  try {
    window.localStorage?.setItem(key, JSON.stringify([...values]));
  } catch {
    // Local persistence is a convenience; the durable event log remains authoritative.
  }
}

function localArtifactKey(sessionId) {
  return `${LOCAL_ARTIFACTS_PREFIX}${sessionId}`;
}

function hiddenArtifactKey(sessionId) {
  return `${LOCAL_HIDDEN_ARTIFACTS_PREFIX}${sessionId}`;
}

function readLocalArtifacts(sessionId) {
  try {
    const value = JSON.parse(window.localStorage?.getItem(localArtifactKey(sessionId)) ?? "[]");
    return Array.isArray(value) ? value : [];
  } catch {
    return [];
  }
}

function writeLocalArtifacts(sessionId) {
  if (!sessionId) return;
  const compact = state.attachments
    .filter((attachment) => attachment.status !== "error")
    .slice(-40)
    .map((attachment) => ({
      id: attachment.id,
      artifactId: attachment.artifactId,
      name: attachment.name,
      mediaType: attachment.mediaType,
      bytes: attachment.bytes,
      isImage: attachment.isImage,
      content: attachment.content,
      contentBase64: attachment.contentBase64,
      kind: attachment.kind,
      createdAt: attachment.createdAt,
      localOnly: attachment.localOnly,
    }));
  try {
    window.localStorage?.setItem(localArtifactKey(sessionId), JSON.stringify(compact));
  } catch {
    // Large artifacts can exceed localStorage. They remain available from the daemon.
  }
}

function showToast(message, kind = "info") {
  toast.textContent = message;
  toast.dataset.kind = kind;
  toast.setAttribute("aria-live", kind === "error" ? "assertive" : "polite");
  toast.classList.add("visible");
  window.clearTimeout(showToast.timer);
  showToast.timer = window.setTimeout(() => toast.classList.remove("visible"), 4200);
}

function displayError(error, fallback = "操作失败，请重试。") {
  const message = error?.message ?? String(error ?? "");
  if (!message) return fallback;
  if (message.startsWith("gateway returned HTTP")) return message.replace("gateway returned HTTP", "HTTP");
  const normalized = message.toLowerCase();
  const knownError = Object.entries(RUNTIME_ERROR_LABELS).find(([key]) => normalized.includes(key));
  return knownError ? knownError[1] : fallback;
}

function resultError(error, fallback) {
  return error ? displayError({ message: String(error) }, fallback) : fallback;
}

function turnParametersSummaryText(parameters) {
  const parts = [parameters.reasoning_effort ? `思考 ${THINKING_EFFORT_LABELS[parameters.reasoning_effort]}` : "思考 自动"];
  if (parameters.temperature !== null) parts.push(`温度 ${parameters.temperature.toFixed(1)}`);
  if (parameters.max_output_tokens !== null) parts.push(`输出 ${parameters.max_output_tokens}`);
  return parts.join(" · ");
}

function renderTurnParameters(parameters = state.turnParameters) {
  const normalized = normalizeTurnParameters(parameters);
  state.turnParameters = normalized;
  thinkingEffortSelect.value = normalized.reasoning_effort ?? "";
  temperatureInput.value = normalized.temperature === null ? "" : String(normalized.temperature);
  maxOutputTokensInput.value = normalized.max_output_tokens === null ? "" : String(normalized.max_output_tokens);
  const summary = turnParametersSummaryText(normalized);
  turnParametersSummary.textContent = summary;
  turnParametersSummary.title = summary;
}

function syncTurnParametersFromForm() {
  const next = normalizeTurnParameters({
    reasoning_effort: thinkingEffortSelect.value,
    temperature: temperatureInput.value,
    max_output_tokens: maxOutputTokensInput.value,
  });
  state.turnParameters = next;
  persistTurnParameters(next);
  renderTurnParameters(next);
}

function turnParametersPayload() {
  const parameters = state.turnParameters;
  return {
    ...(parameters.reasoning_effort ? { reasoning_effort: parameters.reasoning_effort } : {}),
    ...(parameters.temperature !== null ? { temperature: parameters.temperature } : {}),
    ...(parameters.max_output_tokens !== null ? { max_output_tokens: parameters.max_output_tokens } : {}),
  };
}

function escapeHtml(value) {
  return String(value).replace(/[&<>'"]/g, (character) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", "'": "&#39;", '"': "&quot;" })[character]);
}

function normalizedEventType(event) {
  const raw = event?.payload?.type ?? event?.event_type ?? "event";
  return String(raw).replace(/([a-z0-9])([A-Z])/g, "$1_$2").toLowerCase();
}

function eventPayloadData(event) {
  return event?.payload?.data ?? {};
}

function formatBytes(bytes) {
  const amount = Number(bytes) || 0;
  if (amount < 1024) return `${amount} B`;
  if (amount < 1024 * 1024) return `${(amount / 1024).toFixed(amount < 10 * 1024 ? 1 : 0)} KB`;
  return `${(amount / (1024 * 1024)).toFixed(1)} MB`;
}

function formatRelativeTime(timestamp) {
  if (!timestamp) return "刚刚";
  const delta = Math.max(0, Date.now() - Number(timestamp));
  if (delta < 60_000) return "刚刚";
  if (delta < 3_600_000) return `${Math.floor(delta / 60_000)} 分钟前`;
  if (delta < 86_400_000) return `${Math.floor(delta / 3_600_000)} 小时前`;
  if (delta < 7 * 86_400_000) return `${Math.floor(delta / 86_400_000)} 天前`;
  return new Date(timestamp).toLocaleDateString("zh-CN", { month: "short", day: "numeric" });
}

function shortId(value) {
  return String(value ?? "").slice(0, 8) || "未知";
}

function providerKindLabel(kind) {
  return {
    open_ai_compatible: "OpenAI",
    anthropic: "Anthropic",
    openai_codex: "Codex OAuth",
  }[kind] ?? "未知协议";
}

function allEnabledModels(settings) {
  return (settings?.providers ?? []).flatMap((provider) =>
    provider.enabled === false
      ? []
      : (provider.models ?? [])
          .filter((model) => model.enabled !== false)
          .map((model) => ({ ...model, providerName: provider.name })),
  );
}

function selectedModelProfile() {
  return allEnabledModels(state.settings).find((model) => model.id === modelSelect.value);
}

function supportsVision() {
  return Boolean(selectedModelProfile()?.supports_vision);
}

function populateModelSelects(settings) {
  const models = allEnabledModels(settings);
  for (const select of [modelSelect, defaultModelSelect]) {
    const isDefault = select === defaultModelSelect;
    const selected = isDefault ? settings.default_model : (select.value || settings.default_model);
    select.replaceChildren();
    if (isDefault) {
      const empty = document.createElement("option");
      empty.value = "";
      empty.textContent = "不设置默认模型";
      select.append(empty);
    }
    for (const model of models) {
      const option = document.createElement("option");
      option.value = model.id;
      option.textContent = `${model.label || model.id} · ${model.providerName || model.provider_id}`;
      select.append(option);
    }
    if (models.some((model) => model.id === selected)) select.value = selected;
    else if (!isDefault && models[0]) select.value = models[0].id;
    else select.value = "";
  }
}

function renderProviders(settings) {
  const providers = settings?.providers ?? [];
  providerCount.textContent = `${providers.length}`;
  providerList.replaceChildren();
  if (!providers.length) {
    const empty = document.createElement("p");
    empty.className = "timeline-empty";
    empty.textContent = "暂无供应商";
    providerList.append(empty);
    return;
  }
  for (const provider of providers) {
    const item = document.createElement("article");
    item.className = "provider-item";
    const models = (provider.models ?? []).map((model) => model.label || model.id).join("、") || "暂无模型";
    const secret = provider.kind === "openai_codex"
      ? "本地 OAuth"
      : provider.api_key_configured
        ? "已读"
        : "缺少";
    item.innerHTML = `<div class="provider-item-heading"><strong>${escapeHtml(provider.name || provider.id)}</strong><button class="button quiet provider-remove" type="button" data-remove-provider="${escapeHtml(provider.id)}">删除</button></div><span>${escapeHtml(providerKindLabel(provider.kind))}</span><span>${escapeHtml(models)}</span><small>${escapeHtml(secret)}</small>`;
    providerList.append(item);
  }
}

function applySettings(settings) {
  state.settings = settings;
  populateModelSelects(settings);
  renderProviders(settings);
  if (settings.default_model) {
    defaultModelSelect.value = settings.default_model;
    modelSelect.value = settings.default_model;
  }
  $("#model-pill").textContent = modelSelect.value || NO_MODEL_LABEL;
}

function editableSettings() {
  return {
    version: state.settings?.version ?? 1,
    default_model: defaultModelSelect.value || "",
    providers: (state.settings?.providers ?? []).map(({ api_key_configured: _configured, ...provider }) => provider),
  };
}

async function loadSettings() {
  const generation = ++state.settingsLoadGeneration;
  const settings = await client.getSettings();
  if (generation !== state.settingsLoadGeneration) return;
  applySettings(settings);
  settingsStatus.textContent = "已读取";
}

async function saveSettings() {
  state.settingsLoadGeneration += 1;
  const saved = await client.saveSettings(editableSettings());
  applySettings(saved);
  settingsStatus.textContent = "已保存";
  showToast("已保存", "success");
}

function syncProviderEditor() {
  const kind = $("#provider-kind").value;
  const codex = kind === "openai_codex";
  $("#provider-endpoint-label").textContent = codex ? "OAuth" : "接口地址";
  $("#provider-endpoint").placeholder = codex ? "OAuth" : "https://…";
  $("#provider-endpoint").disabled = codex;
  if (codex) $("#provider-endpoint").value = "";
  $("#provider-key-env-label").textContent = codex ? "Secret（不用）" : "Secret 环境变量";
  $("#provider-key-env").disabled = codex;
  if (codex) $("#provider-key-env").value = "";
  $("#provider-model-vision").disabled = false;
  $("#codex-auth-panel").hidden = !codex;
  $("#provider-model-catalog").hidden = true;
  $("#provider-model-catalog").replaceChildren();
  $("#provider-model-discovery-status").textContent = "";
}

async function discoverProviderModels() {
  const providerId = $("#provider-id").value.trim();
  const kind = $("#provider-kind").value;
  const endpoint = $("#provider-endpoint").value.trim();
  const apiKeyEnv = $("#provider-key-env").value.trim();
  if (!providerId) return showToast("请先填写供应商 ID", "warning");
  if (kind !== "openai_codex" && !endpoint) return showToast("请先填写接口地址", "warning");
  const button = $("#discover-provider-models-button");
  const catalog = $("#provider-model-catalog");
  const discoveryStatus = $("#provider-model-discovery-status");
  button.disabled = true;
  discoveryStatus.textContent = "读取模型列表中…";
  try {
    const result = await client.listProviderModels({
      provider_id: providerId,
      kind,
      ...(endpoint ? { endpoint } : {}),
      ...(apiKeyEnv ? { api_key_env: apiKeyEnv } : {}),
    });
    catalog.replaceChildren();
    for (const model of result.models ?? []) {
      const option = document.createElement("option");
      option.value = model.id;
      option.textContent = `${model.label || model.id}${model.supports_vision ? " · 图片" : ""}`;
      option.dataset.label = model.label || model.id;
      option.dataset.supportsVision = model.supports_vision ? "true" : "false";
      catalog.append(option);
    }
    if (!catalog.options.length) {
      catalog.hidden = true;
      discoveryStatus.textContent = "供应商未返回模型";
      return;
    }
    catalog.hidden = false;
    catalog.value = catalog.options[0].value;
    catalog.dispatchEvent(new Event("change"));
    discoveryStatus.textContent = `已找到 ${catalog.options.length} 个模型`;
  } catch (error) {
    catalog.hidden = true;
    discoveryStatus.textContent = "模型列表读取失败";
    showToast(displayError(error, "模型列表读取失败"), "error");
  } finally {
    button.disabled = false;
  }
}

function applyDiscoveredModel() {
  const option = $("#provider-model-catalog").selectedOptions[0];
  if (!option) return;
  $("#provider-model-id").value = option.value;
  $("#provider-model-label").value = option.dataset.label || option.value;
  $("#provider-model-vision").checked = option.dataset.supportsVision === "true";
}

let codexAuthPoll = null;

function renderCodexDeviceCode(deviceCode) {
  const row = $("#codex-device-code-row");
  const code = $("#codex-device-code");
  if (!row || !code) return;
  code.textContent = deviceCode?.user_code ?? "";
  row.hidden = !deviceCode?.user_code;
}

async function copyCodexDeviceCode() {
  const value = $("#codex-device-code")?.textContent?.trim();
  if (!value) return;
  try {
    await navigator.clipboard.writeText(value);
    showToast("设备码已复制", "success");
  } catch {
    showToast("复制失败，请手动选择设备码", "warning");
  }
}

function renderCodexAuthStatus(result) {
  const status = $("#codex-auth-status");
  const error = $("#codex-auth-error");
  renderCodexDeviceCode(result?.device_code);
  if (error) {
    error.textContent = result?.error ?? "";
    error.hidden = !result?.error;
  }
  if (!result?.authenticated && result?.status === "running") {
    status.textContent = "授权中";
    status.className = "pill";
  } else if (result?.authenticated || result?.status === "authenticated") {
    status.textContent = result.expires_at_ms ? `已登录 · ${new Date(result.expires_at_ms).toLocaleTimeString("zh-CN", { hour: "2-digit", minute: "2-digit" })}` : "已登录";
    status.className = "pill";
  } else if (result?.status === "error") {
    status.textContent = "授权失败";
    status.className = "pill neutral";
  } else {
    status.textContent = "未登录";
    status.className = "pill neutral";
  }
}

async function refreshCodexAuthStatus(loginId) {
  try {
    const result = await client.codexAuthStatus(loginId);
    renderCodexAuthStatus(result);
    if (result.status === "error" && result.error) showToast(result.error, "error");
    if (result.status === "authenticated" || result.status === "error" || result.status === "cancelled" || result.authenticated) {
      window.clearInterval(codexAuthPoll);
      codexAuthPoll = null;
    }
    return result;
  } catch (error) {
    renderCodexAuthStatus({ status: "error" });
    showToast(displayError(error, "授权状态读取失败"), "error");
    return null;
  }
}

async function startCodexLogin(method = "browser") {
  const authWindow = method === "browser" ? window.open("about:blank", "_blank") : null;
  if (authWindow) authWindow.opener = null;
  try {
    const result = await client.startCodexLogin(method);
    renderCodexAuthStatus(result);
    if (result.status === "error" && result.error) showToast(result.error, "error");
    const authLink = $("#codex-auth-url");
    if (result.device_code) {
      authLink.href = result.device_code.verification_uri;
      authLink.textContent = `打开设备登录页（设备码：${result.device_code.user_code}）`;
      authLink.hidden = false;
      showToast(`请在设备登录页输入设备码：${result.device_code.user_code}`, "info");
    } else if (result.url) {
      authLink.href = result.url;
      authLink.textContent = "打开授权页面";
      authLink.hidden = false;
      if (authWindow && !authWindow.closed) authWindow.location.replace(result.url);
    }
    window.clearInterval(codexAuthPoll);
    codexAuthPoll = window.setInterval(() => refreshCodexAuthStatus(result.login_id), 1200);
    await refreshCodexAuthStatus(result.login_id);
  } catch (error) {
    const message = displayError(error, "登录失败");
    if (authWindow && !authWindow.closed) {
      authWindow.document.title = "登录失败";
      authWindow.document.body.textContent = message;
    }
    showToast(message, "error");
  }
}

async function logoutCodex() {
  try {
    await client.logoutCodex();
    const authLink = $("#codex-auth-url");
    authLink.hidden = true;
    authLink.textContent = "打开授权页面";
    renderCodexAuthStatus({ authenticated: false });
    showToast("已退出", "success");
  } catch (error) {
    showToast(displayError(error, "退出失败"), "error");
  }
}

function addProviderFromForm() {
  if (!state.settings) return showToast("设置未加载", "warning");
  const id = $("#provider-id").value.trim();
  const name = $("#provider-name").value.trim() || id;
  const kind = $("#provider-kind").value;
  const endpoint = $("#provider-endpoint").value.trim();
  const apiKeyEnv = $("#provider-key-env").value.trim();
  const modelId = $("#provider-model-id").value.trim();
  const modelLabel = $("#provider-model-label").value.trim() || modelId;
  if (!id || !modelId) return showToast("请填写供应商 ID 和模型 ID", "warning");
  if (kind !== "openai_codex" && !endpoint) return showToast("请填写接口地址", "warning");
  const existing = (state.settings.providers ?? []).find((item) => item.id === id);
  const provider = {
    ...(existing ?? {}),
    id,
    name,
    kind,
    endpoint: kind === "openai_codex" ? null : endpoint,
    api_key_env: kind === "openai_codex" ? null : apiKeyEnv || null,
    enabled: true,
    models: [
      ...(existing?.models ?? []).filter((model) => model.id !== modelId),
      { id: modelId, label: modelLabel, provider_id: id, enabled: true, supports_vision: $("#provider-model-vision").checked },
    ],
  };
  const providers = (state.settings.providers ?? []).filter((item) => item.id !== id);
  state.settings = { ...state.settings, providers: [...providers, provider] };
  applySettings(state.settings);
  settingsStatus.textContent = "已添加，保存后生效";
}

function setConnection(online) {
  $("#connection-dot").classList.toggle("online", online);
  $("#connection-label").textContent = online ? "在线" : "离线";
}

function currentSessionSummary() {
  return state.sessions.find((session) => session.id === state.sessionId);
}

function updateSessionChrome() {
  const summary = currentSessionSummary();
  $("#conversation-title").textContent = summary?.title || (state.sessionId ? "新会话" : "选择会话");
  $("#session-id").textContent = state.sessionId
    ? `${summary?.workspaceRoot || "未设置工作区"}`
    : "";
  if (summary?.model && [...modelSelect.options].some((option) => option.value === summary.model)) modelSelect.value = summary.model;
  $("#model-pill").textContent = modelSelect.value || summary?.model || NO_MODEL_LABEL;
  turnContent.disabled = !state.sessionId;
  sendButton.disabled = !state.sessionId || state.uploadingCount > 0 || state.turnInFlight;
  $("#turn-state").textContent = state.turnInFlight ? "运行中" : state.sessionId ? "就绪" : "空闲";
}

function resetSessionView() {
  state.events = [];
  state.subscriptionId = null;
  state.lastOperationId = null;
  state.tools = new Map();
  state.attachments = [];
  state.hiddenArtifactIds = new Set();
  state.queuedAttachmentIds.clear();
  state.messageAttachments.clear();
  state.pendingUserMessage = null;
  state.pendingAssistantMessage = null;
  state.uploadingCount = 0;
  state.turnInFlight = false;
  $("#event-count").textContent = "0";
  $("#artifact-count").textContent = "0";
  renderToolQueue();
  renderAttachmentViews();
  $("#timeline").innerHTML = '<li class="timeline-empty">暂无事件</li>';
  $("#operation-result").textContent = "暂无结果";
  continueButton.disabled = true;
  $("#conversation").innerHTML = '<div class="empty-state"><span class="empty-glyph large" aria-hidden="true">↗</span><strong>选择会话</strong></div>';
}

async function setSession(sessionId, { restore = true } = {}) {
  if (!sessionId) return;
  const generation = ++state.sessionGeneration;
  const previousSubscriptionId = state.subscriptionId;
  client.resetSessionSeen?.(sessionId);
  state.closeStream?.();
  state.closeStream = null;
  state.subscriptionId = null;
  if (previousSubscriptionId) client.unsubscribe({ subscription_id: previousSubscriptionId }).catch(() => undefined);
  state.sessionId = sessionId;
  resetSessionView();
  updateSessionChrome();
  renderSessionHistory();
  if (!restore) return;
  try {
    await subscribeSession(generation);
    if (generation !== state.sessionGeneration) return;
    await loadSessionArtifacts(generation);
    updateSessionChrome();
  } catch (error) {
    if (generation === state.sessionGeneration) showToast(displayError(error, "恢复失败，请重试。"), "error");
  }
}

function makeSessionSummary(id) {
  return {
    id,
    title: "新会话",
    workspaceRoot: ".",
    model: "",
    createdAt: 0,
    updatedAt: 0,
    eventCount: 0,
    turnCount: 0,
    status: "active",
    lastContent: "",
    archived: state.archivedSessionIds.has(id),
  };
}

function applyEventToSessionSummary(summary, event) {
  const type = normalizedEventType(event);
  const data = eventPayloadData(event);
  summary.eventCount += 1;
  summary.updatedAt = Math.max(summary.updatedAt || 0, Number(event.recorded_at_ms) || 0);
  summary.createdAt = summary.createdAt || Number(event.recorded_at_ms) || 0;
  if (type === "session_created") {
    summary.workspaceRoot = data.workspace_roots?.[0] || ".";
    summary.model = data.model || summary.model;
  }
  if (type === "session_forked" && summary.title === "新会话") summary.title = `分支会话 · ${shortId(summary.id)}`;
  if (type === "user_message") {
    summary.turnCount += 1;
    summary.lastContent = String(data.content ?? "").trim().slice(0, 400);
    if (summary.title === "新会话" || summary.title.startsWith("分支会话")) {
      const firstLine = summary.lastContent.split(/\r?\n/, 1)[0].trim();
      if (firstLine) summary.title = firstLine.slice(0, 48);
    }
  }
  if (type === "session_completed") summary.status = "completed";
  summary.archived = state.archivedSessionIds.has(summary.id);
  return summary;
}

async function loadSessions() {
  const generation = ++state.historyLoadGeneration;
  state.historyLoading = true;
  renderSessionHistory();
  try {
    const byId = new Map();
    let after = 0;
    for (let pageNumber = 0; pageNumber < MAX_HISTORY_PAGES; pageNumber += 1) {
      const page = await client.request("runtime.v1.events.replay", { after_global_sequence: after, limit: 1000 });
      for (const event of page?.events ?? []) {
        const id = event.session_id;
        if (!id) continue;
        const summary = byId.get(id) ?? makeSessionSummary(id);
        applyEventToSessionSummary(summary, event);
        byId.set(id, summary);
      }
      if (!page?.has_more || page.next_global_sequence <= after) break;
      after = page.next_global_sequence;
    }
    if (generation !== state.historyLoadGeneration) return;
    const remote = [...byId.values()].filter((session) => !state.hiddenSessionIds.has(session.id));
    const merged = new Map(state.sessions.filter((session) => !state.hiddenSessionIds.has(session.id)).map((session) => [session.id, session]));
    for (const session of remote) merged.set(session.id, session);
    state.sessions = [...merged.values()].sort((left, right) => (right.updatedAt || 0) - (left.updatedAt || 0));
    persistSessionCatalog();
    renderSessionHistory();
    if (state.sessionId && !state.sessions.some((session) => session.id === state.sessionId)) {
      state.sessions.unshift(makeSessionSummary(state.sessionId));
    }
    if (!state.sessionId && state.sessions.length === 1) {
      await setSession(state.sessions[0].id);
    }
  } catch (error) {
    if (generation === state.historyLoadGeneration) {
      state.sessions = state.sessions.filter((session) => !state.hiddenSessionIds.has(session.id));
      persistSessionCatalog();
      renderSessionHistory();
      showToast(displayError(error, "历史读取失败，显示本地记录"), "warning");
    }
  } finally {
    if (generation === state.historyLoadGeneration) {
      state.historyLoading = false;
      renderSessionHistory();
    }
  }
}

function visibleSessions() {
  const query = state.sessionSearch.trim().toLowerCase();
  return state.sessions.filter((session) => {
    if (state.hiddenSessionIds.has(session.id)) return false;
    if (state.sessionFilter === "active" && session.archived) return false;
    if (state.sessionFilter === "archived" && !session.archived) return false;
    if (!query) return true;
    return [session.title, session.workspaceRoot, session.model, session.id].some((value) => String(value ?? "").toLowerCase().includes(query));
  });
}

function renderSessionHistory() {
  const list = $("#session-list");
  const sessions = visibleSessions();
  $("#session-count").textContent = `${state.sessions.filter((session) => !state.hiddenSessionIds.has(session.id)).length} 个会话`;
  list.replaceChildren();
  if (state.historyLoading && !sessions.length) {
    const loading = document.createElement("p");
    loading.className = "timeline-empty";
    loading.textContent = "读取中…";
    list.append(loading);
    return;
  }
  if (!sessions.length) {
    const empty = document.createElement("div");
    empty.className = "history-empty";
    empty.innerHTML = `<span class="empty-glyph" aria-hidden="true">${state.sessionFilter === "archived" ? "□" : "+"}</span><strong>${state.sessionSearch ? "无匹配" : state.sessionFilter === "archived" ? "暂无归档" : "暂无会话"}</strong>`;
    list.append(empty);
    return;
  }
  for (const session of sessions) {
    const item = document.createElement("article");
    item.className = `session-item${session.id === state.sessionId ? " is-active" : ""}${session.archived ? " is-archived" : ""}`;
    const open = document.createElement("button");
    open.className = "session-open";
    open.type = "button";
    open.dataset.openSession = session.id;
    if (session.id === state.sessionId) open.setAttribute("aria-current", "true");
    const title = document.createElement("strong");
    title.textContent = session.title || "新会话";
    const meta = document.createElement("span");
    meta.textContent = `${session.workspaceRoot || "."} · ${session.model || NO_MODEL_LABEL}`;
    const count = document.createElement("small");
    count.textContent = `${session.turnCount} 回合`;
    open.append(title, meta, count);
    const row = document.createElement("div");
    row.className = "session-item-meta";
    const status = document.createElement("span");
    status.className = `pill ${session.archived ? "neutral" : ""}`;
    status.textContent = session.archived ? "已归档" : session.status === "completed" ? "已完成" : "活跃";
    const time = document.createElement("time");
    time.dateTime = session.updatedAt ? new Date(session.updatedAt).toISOString() : "";
    time.textContent = formatRelativeTime(session.updatedAt || session.createdAt);
    row.append(status, time);
    const actions = document.createElement("div");
    actions.className = "session-item-actions";
    const archive = document.createElement("button");
    archive.type = "button";
    archive.dataset.archiveSession = session.id;
    archive.textContent = session.archived ? "取消归档" : "归档";
    archive.setAttribute("aria-label", `${archive.textContent} ${session.title}`);
    const remove = document.createElement("button");
    remove.type = "button";
    remove.className = "danger";
    remove.dataset.hideSession = session.id;
    remove.textContent = "移除";
    remove.setAttribute("aria-label", `从列表移除 ${session.title}`);
    actions.append(archive, remove);
    item.append(open, row, actions);
    list.append(item);
  }
}

async function createSessionFromForm(event) {
  event.preventDefault();
  const selectedModel = modelSelect.value.trim();
  if (!selectedModel) return showToast("请先在设置中添加并选择模型", "warning");
  const button = createForm.querySelector('button[type="submit"]');
  button.disabled = true;
  button.textContent = "创建中…";
  try {
    const result = await client.createSession({
      command_id: crypto.randomUUID(),
      workspace_roots: [$("#workspace-root").value.trim() || "."],
      model: selectedModel,
    });
    setConnection(true);
    const sessionId = result.session_id;
    const summary = makeSessionSummary(sessionId);
    summary.workspaceRoot = $("#workspace-root").value.trim() || ".";
    summary.model = selectedModel;
    applyEventToSessionSummary(summary, result.event);
    state.sessions = [summary, ...state.sessions.filter((session) => session.id !== sessionId)];
    persistSessionCatalog();
    await setSession(sessionId, { restore: false });
    addEvent(result.event);
    await subscribeSession(state.sessionGeneration);
    sessionDialog.close();
    renderSessionHistory();
    showToast("已创建", "success");
  } catch (error) {
    setConnection(false);
    showToast(displayError(error), "error");
  } finally {
    button.disabled = false;
    button.textContent = "创建会话";
  }
}

function toolIntentLabel(intent) {
  const kind = intent?.kind ?? "unknown";
  const data = intent?.data ?? {};
  const labels = {
    readfile: `读取 ${data.path ?? "文件"}`,
    search: `搜索 ${data.root ?? "工作区"}`,
    listfiles: `列出 ${data.root ?? "目录"}`,
    readimage: `看图 ${data.path ?? "文件"}`,
    writefile: `写入 ${data.path ?? "文件"}`,
    editfile: `编辑 ${data.path ?? "文件"}`,
    exec: `运行 ${data.program ?? "程序"}`,
    test: `测试 ${data.program ?? "程序"}`,
    git: "Git 操作",
  };
  return labels[kind.replaceAll("_", "").toLowerCase()] ?? "工具请求";
}

function toolStatusLabel(status) {
  return { pending: "待执行", approval: "待审批", running: "执行中", done: "完成", failed: "失败" }[status] ?? "待执行";
}

function renderToolQueue() {
  const tools = [...state.tools.values()];
  toolCount.textContent = `${tools.length}`;
  toolQueue.replaceChildren();
  if (!tools.length) {
    const empty = document.createElement("p");
    empty.className = "timeline-empty";
    empty.textContent = "暂无请求";
    toolQueue.append(empty);
    return;
  }
  for (const tool of tools) {
    const item = document.createElement("article");
    item.className = "tool-item";
    item.dataset.status = tool.status;
    const heading = document.createElement("div");
    heading.className = "tool-item-heading";
    const title = document.createElement("strong");
    title.textContent = tool.toolName;
    const status = document.createElement("span");
    status.className = "pill neutral";
    status.textContent = toolStatusLabel(tool.status);
    heading.append(title, status);
    const detail = document.createElement("small");
    detail.textContent = toolIntentLabel(tool.intent);
    item.append(heading, detail);
    if (["pending", "approval"].includes(tool.status)) {
      const button = document.createElement("button");
      button.className = "button secondary compact-button";
      button.type = "button";
      button.dataset.executeTool = tool.operationId;
      button.textContent = tool.status === "approval" ? "批准" : "执行";
      item.append(button);
    }
    if (tool.resultText) {
      const result = document.createElement("pre");
      result.className = "tool-result";
      result.textContent = tool.resultText;
      item.append(result);
    }
    if (tool.imageDataUrl) {
      const preview = document.createElement("button");
      preview.type = "button";
      preview.className = "media-preview-button";
      preview.dataset.previewTool = tool.operationId;
      preview.setAttribute("aria-label", "预览工具图片");
      const image = document.createElement("img");
      image.className = "tool-preview";
      image.alt = "工具图片";
      image.loading = "lazy";
      image.decoding = "async";
      image.src = tool.imageDataUrl;
      preview.append(image);
      item.append(preview);
    }
    toolQueue.append(item);
  }
}

function updateToolFromEvent(event) {
  const type = normalizedEventType(event);
  const data = eventPayloadData(event);
  const operationId = data.operation_id;
  if (!operationId) return;
  const existing = state.tools.get(operationId) ?? {
    operationId,
    toolName: data.tool_name ?? "工具",
    intent: data.intent,
    turnId: data.turn_id,
    runId: data.run_id,
    status: "pending",
  };
  if (type === "tool_proposed") Object.assign(existing, { toolName: data.tool_name ?? existing.toolName, intent: data.intent, turnId: data.turn_id, runId: data.run_id, status: "pending" });
  if (type === "approval_requested" || type === "tool_approval_requested") Object.assign(existing, { requestDigest: data.request_digest, status: "approval" });
  if (type === "tool_started") existing.status = "running";
  if (type === "tool_finished") existing.status = "done";
  if (type === "tool_failed" || type === "execution_unknown") existing.status = "failed";
  if (type === "artifact_created") existing.artifactId = data.artifact_id;
  state.tools.set(operationId, existing);
  renderToolQueue();
}

function appendToolResult(operationId, result) {
  const tool = state.tools.get(operationId);
  if (!tool) return;
  const output = result?.result ?? result;
  tool.status = result?.accepted === false ? "approval" : "done";
  tool.resultText = output?.stdout || output?.stderr || (result?.error ? result.error : "已完成，无输出");
  if (output?.output_media_type && output?.output_encoding === "base64" && output.stdout) {
    tool.imageDataUrl = `data:${output.output_media_type};base64,${output.stdout}`;
    tool.resultText = `已读取图片 · ${output.output_media_type}`;
  }
  renderToolQueue();
}

function formatType(event) {
  const type = normalizedEventType(event);
  return EVENT_TYPE_LABELS[type] ?? (type === "tool_proposed" ? "工具请求" : "事件");
}

function renderTimeline() {
  const timeline = $("#timeline");
  timeline.replaceChildren();
  for (const item of state.events.slice(-80)) {
    const entry = document.createElement("li");
    entry.className = "timeline-item";
    const payload = eventPayloadData(item);
    const sequence = document.createElement("span");
    sequence.className = "timeline-seq";
    sequence.textContent = item.global_sequence ?? "·";
    const details = document.createElement("div");
    const type = document.createElement("strong");
    type.textContent = formatType(item);
    const correlation = document.createElement("span");
    correlation.textContent = payload.operation_id ?? payload.turn_id ?? payload.run_id ?? payload.artifact_id ?? "已提交";
    details.append(type, correlation);
    entry.append(sequence, details);
    timeline.append(entry);
  }
  if (!state.events.length) {
    const empty = document.createElement("li");
    empty.className = "timeline-empty";
    empty.textContent = "暂无事件";
    timeline.append(empty);
  }
  $("#event-count").textContent = `${state.events.length}`;
}

function attachmentById(id) {
  return state.attachments.find((attachment) => attachment.id === id || attachment.artifactId === id);
}

function attachmentsFromMessage(content) {
  const ids = [...String(content ?? "").matchAll(/artifact_id=([^\]\s]+)/g)].map((match) => match[1]);
  return ids.map(attachmentById).filter(Boolean);
}

function createAttachmentThumb(attachment, { button = false } = {}) {
  const element = document.createElement(button ? "button" : "div");
  element.className = "attachment-thumb";
  if (button) {
    element.type = "button";
    element.dataset.previewAttachment = attachment.id;
    element.setAttribute("aria-label", `预览 ${attachment.name}`);
  }
  if (attachment.isImage && attachment.previewUrl) {
    const image = document.createElement("img");
    image.src = attachment.previewUrl;
    image.alt = `${attachment.name} 图片预览`;
    image.loading = "lazy";
    image.decoding = "async";
    element.append(image);
  } else {
    element.textContent = (attachment.name.split(".").pop() || "FILE").slice(0, 5).toUpperCase();
  }
  return element;
}

function attachmentStatus(attachment) {
  if (attachment.status === "uploading") return "保存中…";
  if (attachment.status === "error") return attachment.error || "上传失败";
  if (attachment.localOnly) return "仅本地";
  if (attachment.isImage) return "会话内 · 支持视觉";
  return `会话内 · ${formatBytes(attachment.bytes)}`;
}

function createAttachmentCard(attachment, { compact = false, queued = false } = {}) {
  const card = document.createElement(compact ? "div" : "article");
  card.className = `attachment-card${attachment.status === "uploading" ? " is-uploading" : ""}${attachment.status === "error" ? " is-error" : ""}`;
  card.dataset.attachmentId = attachment.id;
  card.append(createAttachmentThumb(attachment, { button: attachment.isImage }));
  const info = document.createElement("div");
  info.className = "attachment-info";
  const name = document.createElement("strong");
  name.title = attachment.name;
  name.textContent = attachment.name;
  const status = document.createElement("span");
  status.textContent = compact && queued ? "待发送 · " + formatBytes(attachment.bytes) : attachmentStatus(attachment);
  info.append(name, status);
  card.append(info);
  if (!compact) {
    const actions = document.createElement("div");
    actions.className = "attachment-actions";
    if (!attachment.isImage && attachment.status !== "uploading" && attachment.status !== "error") {
      const save = document.createElement("button");
      save.type = "button";
      save.dataset.saveAttachment = attachment.id;
      save.textContent = "写入";
      actions.append(save);
    }
    if (attachment.isImage && attachment.status !== "uploading") {
      const preview = document.createElement("button");
      preview.type = "button";
      preview.dataset.previewAttachment = attachment.id;
      preview.textContent = "预览";
      actions.append(preview);
    }
    const remove = document.createElement("button");
    remove.type = "button";
    remove.dataset.removeAttachment = attachment.id;
    remove.textContent = "移除";
    remove.setAttribute("aria-label", `${remove.textContent} ${attachment.name}`);
    actions.append(remove);
    card.append(actions);
  } else {
    const remove = document.createElement("button");
    remove.type = "button";
    remove.className = "quiet attachment-remove";
    remove.dataset.removeAttachment = attachment.id;
    remove.textContent = "×";
    remove.setAttribute("aria-label", `移除待发送附件 ${attachment.name}`);
    card.append(remove);
  }
  return card;
}

function renderAttachmentViews() {
  const composer = $("#composer-attachments");
  const artifacts = $("#artifact-list");
  const isHidden = (attachment) => state.hiddenArtifactIds.has(attachment.id) || state.hiddenArtifactIds.has(attachment.artifactId);
  const queued = state.attachments.filter((attachment) => state.queuedAttachmentIds.has(attachment.id) && attachment.status !== "error" && !isHidden(attachment));
  const visibleArtifacts = state.attachments.filter((attachment) => !isHidden(attachment));
  composer.replaceChildren();
  artifacts.replaceChildren();
  if (queued.length) {
    for (const attachment of queued) composer.append(createAttachmentCard(attachment, { compact: true, queued: true }));
  }
  if (!visibleArtifacts.length) {
    const empty = document.createElement("p");
    empty.className = "timeline-empty";
    empty.textContent = "暂无附件";
    artifacts.append(empty);
  } else {
    for (const attachment of visibleArtifacts.slice().reverse()) artifacts.append(createAttachmentCard(attachment));
  }
  $("#artifact-count").textContent = `${state.attachments.filter((attachment) => !state.hiddenArtifactIds.has(attachment.id) && !state.hiddenArtifactIds.has(attachment.artifactId)).length}`;
  sendButton.disabled = !state.sessionId || state.uploadingCount > 0 || state.turnInFlight;
}

function base64FromBytes(bytes) {
  let binary = "";
  const chunkSize = 0x8000;
  for (let index = 0; index < bytes.length; index += chunkSize) {
    binary += String.fromCharCode(...bytes.subarray(index, index + chunkSize));
  }
  return btoa(binary);
}

async function fileToBase64(file) {
  return base64FromBytes(new Uint8Array(await file.arrayBuffer()));
}

function isSupportedImage(file) {
  return ["image/png", "image/jpeg", "image/webp", "image/gif"].includes(file.type.toLowerCase());
}

function looksLikeText(file) {
  return file.type.startsWith("text/") || /\.(txt|md|markdown|json|js|mjs|ts|tsx|jsx|css|html|xml|yaml|yml|toml|rs|py|go|java|sql|sh|ps1|csv|log)$/i.test(file.name);
}

function artifactKindFor(name, isImage) {
  return `attachment:${isImage ? "image" : "text"}:${encodeURIComponent(name)}`;
}

function parseArtifactKind(kind, fallbackName = "附件") {
  const parts = String(kind ?? "").split(":");
  if (parts[0] !== "attachment") return { name: fallbackName, isImage: false };
  let name = fallbackName;
  try { name = decodeURIComponent(parts.slice(2).join(":") || fallbackName); } catch { /* keep fallback */ }
  return { name, isImage: parts[1] === "image" };
}

function attachmentFromLocal(record) {
  const isImage = Boolean(record.isImage || String(record.mediaType).startsWith("image/"));
  const contentBase64 = record.contentBase64 || (isImage ? record.content : "");
  return {
    id: record.id || record.artifactId || `local-${crypto.randomUUID()}`,
    artifactId: record.artifactId || record.id || null,
    name: record.name || "附件",
    mediaType: record.mediaType || (isImage ? "image/png" : "text/plain"),
    bytes: Number(record.bytes) || (isImage ? Math.floor(contentBase64.length * .75) : String(record.content || "").length),
    isImage,
    content: isImage ? "" : String(record.content || ""),
    contentBase64,
    previewUrl: isImage && contentBase64 ? `data:${record.mediaType || "image/png"};base64,${contentBase64}` : "",
    status: record.status || "ready",
    error: record.error || "",
    localOnly: Boolean(record.localOnly),
    kind: record.kind || artifactKindFor(record.name || "附件", isImage),
    createdAt: record.createdAt || Date.now(),
  };
}

function attachmentFromMetadata(metadata, content, status = "ready", error = "") {
  const fallbackName = metadata.kind === "image_base64" ? "工具图片" : "附件";
  const parsed = parseArtifactKind(metadata.kind, fallbackName);
  const isImage = parsed.isImage || String(metadata.media_type).startsWith("image/");
  return attachmentFromLocal({
    id: metadata.artifact_id,
    artifactId: metadata.artifact_id,
    name: parsed.name,
    mediaType: metadata.media_type,
    bytes: isImage ? Math.floor(Number(metadata.bytes || 0) * .75) : metadata.bytes,
    isImage,
    content: isImage ? "" : content,
    contentBase64: isImage ? content : "",
    kind: metadata.kind,
    status,
    error,
    createdAt: metadata.created_at_ms,
  });
}

async function loadSessionArtifacts(generation) {
  const sessionId = state.sessionId;
  const stale = () => generation !== state.sessionGeneration || state.sessionId !== sessionId;
  state.hiddenArtifactIds = loadStringSet(hiddenArtifactKey(sessionId));
  const local = readLocalArtifacts(sessionId);
  state.attachments = local
    .map(attachmentFromLocal)
    .filter((attachment) => !state.hiddenArtifactIds.has(attachment.id) && !state.hiddenArtifactIds.has(attachment.artifactId));
  renderAttachmentViews();
  try {
    const result = await client.listArtifacts({ session_id: sessionId });
    if (stale()) return;
    for (const metadata of result?.artifacts ?? result?.items ?? []) {
      const kind = String(metadata.kind ?? "");
      const supported = kind.startsWith("attachment:") || kind === "image_base64";
      if (!supported || stale() || state.hiddenArtifactIds.has(metadata.artifact_id) || state.attachments.some((attachment) => attachment.artifactId === metadata.artifact_id)) continue;
      try {
        const value = await client.getArtifact({ session_id: sessionId, artifact_id: metadata.artifact_id, offset: 0, limit: 1_500_000 });
        if (stale()) return;
        const content = value?.content ?? "";
        state.attachments.push(attachmentFromMetadata(metadata, content));
      } catch {
        if (stale()) return;
        state.attachments.push(attachmentFromMetadata(metadata, "", "error", "内容暂不可用"));
      }
    }
  } catch {
    // Older daemons do not expose artifact.list yet. Local cache remains usable.
  }
  if (!stale()) {
    writeLocalArtifacts(sessionId);
    renderAttachmentViews();
    renderConversation();
  }
}

async function addOneFile(file) {
  const sessionId = state.sessionId;
  const generation = state.sessionGeneration;
  if (!sessionId) return showToast("先选择会话", "warning");
  if (file.size > MAX_ATTACHMENT_BYTES) return showToast(`${file.name} 超过 ${formatBytes(MAX_ATTACHMENT_BYTES)}`, "warning");
  const isImage = isSupportedImage(file);
  if (!isImage && !looksLikeText(file)) return showToast(`不支持此文件：${file.name}`, "warning");
  const id = `pending-${crypto.randomUUID()}`;
  const attachment = {
    id,
    artifactId: null,
    name: file.name,
    mediaType: file.type || (isImage ? "image/png" : "text/plain"),
    bytes: file.size,
    isImage,
    content: "",
    contentBase64: "",
    previewUrl: "",
    status: "uploading",
    localOnly: false,
    kind: artifactKindFor(file.name, isImage),
    createdAt: Date.now(),
  };
  state.attachments.push(attachment);
  state.queuedAttachmentIds.add(id);
  state.uploadingCount += 1;
  renderAttachmentViews();
  try {
    if (isImage) {
      attachment.contentBase64 = await fileToBase64(file);
      attachment.previewUrl = `data:${attachment.mediaType};base64,${attachment.contentBase64}`;
    } else {
      const text = await file.text();
      if (text.length > MAX_TEXT_ATTACHMENT_CHARS) {
        attachment.status = "error";
        attachment.error = `超过 ${MAX_TEXT_ATTACHMENT_CHARS.toLocaleString()} 字符`;
        return;
      }
      attachment.content = text;
    }
    if (state.sessionId !== sessionId || state.sessionGeneration !== generation) return;
    const result = await client.putArtifact({
      session_id: sessionId,
      operation_id: `client-upload-${crypto.randomUUID()}`,
      kind: attachment.kind,
      media_type: attachment.mediaType,
      content: isImage ? attachment.contentBase64 : attachment.content,
    });
    const metadata = result?.metadata ?? result;
    attachment.artifactId = metadata?.artifact_id || attachment.id;
    const previousId = attachment.id;
    attachment.id = attachment.artifactId;
    if (state.queuedAttachmentIds.has(previousId)) {
      state.queuedAttachmentIds.delete(previousId);
      state.queuedAttachmentIds.add(attachment.id);
    }
    if (!attachment.isImage) attachment.bytes = Number(metadata?.bytes) || attachment.bytes;
    attachment.status = "ready";
  } catch (error) {
    attachment.status = "ready";
    attachment.localOnly = true;
    attachment.artifactId = attachment.id;
    if (state.sessionId === sessionId && state.sessionGeneration === generation) {
      showToast(`${file.name} 已保存在本地：${displayError(error, "服务不可用")}`, "warning");
    }
  } finally {
    if (state.sessionId === sessionId && state.sessionGeneration === generation) {
      state.uploadingCount = Math.max(0, state.uploadingCount - 1);
      writeLocalArtifacts(sessionId);
      renderAttachmentViews();
    }
  }
}

async function addFiles(fileList) {
  const files = [...(fileList ?? [])];
  const sessionId = state.sessionId;
  const generation = state.sessionGeneration;
  if (!files.length) return;
  for (const file of files.slice(0, 8)) {
    if (state.sessionId !== sessionId || state.sessionGeneration !== generation) break;
    await addOneFile(file);
  }
}

function composeTurnContent(content, attachments) {
  const parts = [];
  if (content.trim()) parts.push(content.trim());
  for (const attachment of attachments) {
    const id = attachment.artifactId || attachment.id;
    if (attachment.isImage) parts.push(`[图片附件: ${attachment.name}; artifact_id=${id}]`);
    else parts.push(`[文本附件: ${attachment.name}; artifact_id=${id}]\n${attachment.content.slice(0, MAX_TEXT_ATTACHMENT_CHARS)}\n[/文本附件]`);
  }
  return parts.join("\n\n");
}

function compactMessageContent(role, content) {
  let value = String(content ?? "");
  if (role === "user") {
    value = value
      .replace(/\[文本附件:[^\]]+\][\s\S]*?\[\/文本附件\]/g, "")
      .replace(/\[图片附件:[^\]]+\]/g, "")
      .trim();
  }
  const limit = role === "user" ? 6_000 : 20_000;
  return value.length > limit ? `${value.slice(0, limit)}\n… 已截断，完整内容在事件中` : value;
}

function renderMessageElement(role, content, meta = "", attachments = []) {
  const message = document.createElement("article");
  message.className = `message ${role}`;
  const label = document.createElement("div");
  label.className = "message-label";
  label.textContent = role === "user" ? "你" : role === "tool" ? "工具" : "智能体";
  message.append(label);
  if (attachments.length) {
    const attachmentList = document.createElement("div");
    attachmentList.className = "message-attachments";
    for (const attachment of attachments) {
      const item = document.createElement("div");
      item.className = "message-attachment";
      item.append(createAttachmentThumb(attachment, { button: attachment.isImage }));
      const name = document.createElement("strong");
      name.title = attachment.name;
      name.textContent = attachment.name;
      item.append(name);
      attachmentList.append(item);
    }
    message.append(attachmentList);
  }
  const bodyText = compactMessageContent(role, content);
  if (bodyText) {
    const body = document.createElement("p");
    body.textContent = bodyText;
    message.append(body);
  }
  if (meta) {
    const footer = document.createElement("span");
    footer.className = "message-meta";
    footer.textContent = STOP_REASON_LABELS[meta] ?? meta;
    message.append(footer);
  }
  return message;
}

function renderConversation() {
  const conversation = $("#conversation");
  conversation.replaceChildren();
  const messageEvents = state.events.filter((event) => ["user_message", "model_responded"].includes(normalizedEventType(event)));
  if (!messageEvents.length && !state.pendingUserMessage && !state.pendingAssistantMessage) {
    const emptyText = state.sessionId ? "暂无消息，输入即可开始" : "选择会话";
    conversation.innerHTML = `<div class="empty-state"><span class="empty-glyph large" aria-hidden="true">↗</span><strong>${emptyText}</strong></div>`;
    return;
  }
  for (const event of messageEvents) {
    const type = normalizedEventType(event);
    const data = eventPayloadData(event);
    if (type === "user_message") {
      conversation.append(renderMessageElement("user", data.content ?? "", "", state.messageAttachments.get(data.turn_id) ?? attachmentsFromMessage(data.content)));
    } else {
      conversation.append(renderMessageElement("assistant", data.content ?? "", data.stop_reason ?? "completed"));
    }
  }
  if (state.pendingUserMessage && !messageEvents.some((event) => eventPayloadData(event).turn_id === state.pendingUserMessage.turnId)) {
    conversation.append(renderMessageElement("user", state.pendingUserMessage.displayContent, "", state.pendingUserMessage.attachments));
  }
  if (state.pendingAssistantMessage && !messageEvents.some((event) => eventPayloadData(event).turn_id === state.pendingAssistantMessage.turnId && normalizedEventType(event) === "model_responded")) {
    conversation.append(renderMessageElement("assistant", state.pendingAssistantMessage.content, "运行中"));
  }
  conversation.lastElementChild?.scrollIntoView({ block: "nearest", behavior: prefersReducedMotion() ? "auto" : "smooth" });
}

function addEvent(event, generation = state.sessionGeneration) {
  if (!event || generation !== state.sessionGeneration || event.session_id !== state.sessionId || state.events.some((item) => item.event_id === event.event_id)) return;
  updateToolFromEvent(event);
  const type = normalizedEventType(event);
  const data = eventPayloadData(event);
  if (type === "user_message" && state.pendingUserMessage?.turnId === data.turn_id) state.pendingUserMessage = null;
  if (type === "model_responded" && state.pendingAssistantMessage?.turnId === data.turn_id) state.pendingAssistantMessage = null;
  state.events.push(event);
  state.events.sort((left, right) => left.global_sequence - right.global_sequence);
  const summary = currentSessionSummary();
  if (summary) {
    applyEventToSessionSummary(summary, event);
    state.sessions = [summary, ...state.sessions.filter((session) => session.id !== summary.id)].sort((left, right) => (right.updatedAt || 0) - (left.updatedAt || 0));
    persistSessionCatalog();
  }
  renderTimeline();
  renderConversation();
  renderSessionHistory();
}

function prefersReducedMotion() {
  return window.matchMedia?.("(prefers-reduced-motion: reduce)").matches ?? false;
}

async function subscribeSession(generation = state.sessionGeneration) {
  state.closeStream?.();
  state.closeStream = null;
  const result = await client.subscribe({ session_id: state.sessionId, after_global_sequence: 0, limit: 1000 });
  if (generation !== state.sessionGeneration) return;
  state.subscriptionId = result.subscription_id;
  for (const event of result.events ?? []) addEvent(event, generation);
  if (state.subscriptionId && result.next_global_sequence) {
    await client.ackEvents({ subscription_id: state.subscriptionId, after_global_sequence: result.next_global_sequence }).catch(() => undefined);
  }
  state.closeStream = client.streamEvents({
    sessionId: state.sessionId,
    subscriptionId: state.subscriptionId,
    afterGlobalSequence: result.next_global_sequence ?? 0,
    onEvent: (event) => {
      if (generation !== state.sessionGeneration || event.session_id !== state.sessionId) return;
      addEvent(event, generation);
      if (state.subscriptionId && event.global_sequence) {
        client.ackEvents({ subscription_id: state.subscriptionId, after_global_sequence: event.global_sequence }).catch(() => undefined);
      }
    },
    onError: () => {
      if (generation === state.sessionGeneration) showToast("事件流重连中", "warning");
    },
  });
}

async function refreshSessionEvents(expectedGeneration = state.sessionGeneration, expectedSessionId = state.sessionId) {
  if (!expectedSessionId || expectedGeneration !== state.sessionGeneration || state.sessionId !== expectedSessionId) return false;
  const after = state.events.at(-1)?.global_sequence ?? 0;
  const result = await client.request("runtime.v1.session.events", { session_id: expectedSessionId, after_global_sequence: after, limit: 1000 });
  if (expectedGeneration !== state.sessionGeneration || state.sessionId !== expectedSessionId) return false;
  for (const event of result?.events ?? []) addEvent(event, expectedGeneration);
  return true;
}

function displayContentForAttachments(content, attachments) {
  return content.trim() || (attachments.length ? `发送 ${attachments.length} 个附件` : "");
}

turnForm.addEventListener("submit", async (event) => {
  event.preventDefault();
  const content = turnContent.value.trim();
  const queued = state.attachments.filter((attachment) => state.queuedAttachmentIds.has(attachment.id) && attachment.status !== "error" && !state.hiddenArtifactIds.has(attachment.id) && !state.hiddenArtifactIds.has(attachment.artifactId));
  const sessionId = state.sessionId;
  const generation = state.sessionGeneration;
  if (!sessionId) return showToast("先选择会话", "warning");
  const selectedModel = modelSelect.value.trim();
  if (!selectedModel) return showToast("请先在设置中添加并选择模型", "warning");
  if (state.turnInFlight) return showToast("回合运行中", "warning");
  if (state.uploadingCount) return showToast("附件保存中", "warning");
  if (!content && !queued.length) return;
  if (queued.some((attachment) => attachment.isImage) && !supportsVision()) {
    return showToast("当前模型不支持图片", "warning");
  }
  syncTurnParametersFromForm();
  const turnId = crypto.randomUUID();
  const fullContent = composeTurnContent(content, queued);
  const imageAttachments = queued.filter((attachment) => attachment.isImage).map((attachment) => ({
    name: attachment.name,
    media_type: attachment.mediaType,
    content_base64: attachment.contentBase64,
  }));
  const imagePayloadSize = imageAttachments.reduce((total, attachment) => total + attachment.content_base64.length, 0);
  if (imagePayloadSize > 1_500_000) return showToast("图片过大", "warning");
  if (new TextEncoder().encode(fullContent).length + imagePayloadSize > 1_500_000) return showToast("消息过大", "warning");
  const displayContent = displayContentForAttachments(content, queued);
  state.turnInFlight = true;
  state.messageAttachments.set(turnId, queued.slice());
  state.pendingUserMessage = { turnId, displayContent, attachments: queued.slice() };
  state.pendingAssistantMessage = null;
  renderConversation();
  sendButton.disabled = true;
  $("#turn-state").textContent = "运行中";
  turnContent.value = "";
  try {
    const result = await client.startTurn({
      command_id: crypto.randomUUID(),
      session_id: sessionId,
      turn_id: turnId,
      content: fullContent,
      attachments: imageAttachments,
      model: selectedModel,
      parameters: turnParametersPayload(),
    });
    if (generation !== state.sessionGeneration || state.sessionId !== sessionId) return;
    if (result.response?.content && !state.events.some((item) => normalizedEventType(item) === "model_responded" && eventPayloadData(item).turn_id === turnId)) {
      state.pendingAssistantMessage = { turnId, content: result.response.content };
      renderConversation();
    }
    if (!result.accepted) showToast(resultError(result.error, "需恢复"), "warning");
    else {
      for (const attachment of queued) state.queuedAttachmentIds.delete(attachment.id);
      writeLocalArtifacts(sessionId);
      renderAttachmentViews();
      showToast("已提交", "success");
    }
  } catch (error) {
    if (generation === state.sessionGeneration && state.sessionId === sessionId) showToast(displayError(error), "error");
  } finally {
    if (generation === state.sessionGeneration && state.sessionId === sessionId) {
      state.turnInFlight = false;
      $("#turn-state").textContent = "就绪";
      updateSessionChrome();
    }
  }
});

turnContent.addEventListener("keydown", (event) => {
  if ((event.ctrlKey || event.metaKey) && event.key === "Enter") turnForm.requestSubmit();
});

$("#pick-files-button").addEventListener("click", () => $("#file-input").click());
$("#file-input").addEventListener("change", async (event) => {
  await addFiles(event.target.files);
  event.target.value = "";
});

const dropZone = $("#drop-zone");
dropZone.addEventListener("dragover", (event) => {
  if (!event.dataTransfer?.types?.includes("Files")) return;
  event.preventDefault();
  event.dataTransfer.dropEffect = "copy";
  dropZone.classList.add("is-dragging");
});
dropZone.addEventListener("dragleave", () => dropZone.classList.remove("is-dragging"));
dropZone.addEventListener("drop", async (event) => {
  if (!event.dataTransfer?.files?.length) return;
  event.preventDefault();
  dropZone.classList.remove("is-dragging");
  await addFiles(event.dataTransfer.files);
});
async function handlePastedFiles(event) {
  if (event.defaultPrevented) return false;
  const files = [...(event.clipboardData?.files ?? [])];
  if (!files.length) return false;
  event.preventDefault();
  await addFiles(files);
  return true;
}

dropZone.addEventListener("paste", handlePastedFiles);
turnForm.addEventListener("paste", handlePastedFiles);

$("#session-list").addEventListener("click", async (event) => {
  const open = event.target.closest("[data-open-session]");
  if (open) return setSession(open.dataset.openSession);
  const archive = event.target.closest("[data-archive-session]");
  if (archive) {
    const id = archive.dataset.archiveSession;
    if (state.archivedSessionIds.has(id)) state.archivedSessionIds.delete(id);
    else state.archivedSessionIds.add(id);
    persistStringSet(ARCHIVED_SESSIONS_KEY, state.archivedSessionIds);
    const session = state.sessions.find((item) => item.id === id);
    if (session) session.archived = state.archivedSessionIds.has(id);
    renderSessionHistory();
    showToast(state.archivedSessionIds.has(id) ? "已归档" : "已恢复", "success");
    return;
  }
  const hide = event.target.closest("[data-hide-session]");
  if (hide) {
    const id = hide.dataset.hideSession;
    state.hiddenSessionIds.add(id);
    persistStringSet(HIDDEN_SESSIONS_KEY, state.hiddenSessionIds);
    renderSessionHistory();
    showToast("已移除", "info");
  }
});

$("#session-search").addEventListener("input", (event) => {
  state.sessionSearch = event.target.value;
  renderSessionHistory();
});
for (const button of document.querySelectorAll("[data-history-filter]")) {
  button.addEventListener("click", () => {
    state.sessionFilter = button.dataset.historyFilter;
    for (const sibling of document.querySelectorAll("[data-history-filter]")) {
      const active = sibling === button;
      sibling.classList.toggle("is-active", active);
      sibling.setAttribute("aria-pressed", String(active));
    }
    renderSessionHistory();
  });
}

$("#new-session-button").addEventListener("click", () => {
  if (!sessionDialog.open) sessionDialog.showModal();
});
$("#refresh-sessions-button").addEventListener("click", () => loadSessions());
for (const selector of ["#session-dialog-close", "#session-dialog-cancel"]) $(selector).addEventListener("click", () => sessionDialog.close());
createForm.addEventListener("submit", createSessionFromForm);

function openMediaPreview(attachment) {
  if (!attachment?.previewUrl) return;
  if (!previewDialog) return window.open(attachment.previewUrl, "_blank", "noopener,noreferrer");
  $("#media-preview-title").textContent = attachment.name;
  $("#media-preview-image").src = attachment.previewUrl;
  $("#media-preview-image").alt = `${attachment.name} 图片预览`;
  $("#media-preview-meta").textContent = `${formatBytes(attachment.bytes)} · ${attachment.mediaType}`;
  if (!previewDialog.open) previewDialog.showModal();
}

for (const container of [$("#artifact-list"), $("#composer-attachments"), $("#conversation"), toolQueue]) {
  container.addEventListener("click", async (event) => {
    const preview = event.target.closest("[data-preview-attachment]");
    if (preview) return openMediaPreview(attachmentById(preview.dataset.previewAttachment));
    const remove = event.target.closest("[data-remove-attachment]");
    if (remove) {
      const attachmentId = remove.dataset.removeAttachment;
      const attachment = attachmentById(attachmentId);
      if (container === $("#artifact-list")) {
        state.hiddenArtifactIds.add(attachmentId);
        if (attachment?.artifactId) state.hiddenArtifactIds.add(attachment.artifactId);
        state.queuedAttachmentIds.delete(attachmentId);
        persistStringSet(hiddenArtifactKey(state.sessionId), state.hiddenArtifactIds);
      } else {
        state.attachments = state.attachments.filter((item) => item.id !== attachmentId);
        state.queuedAttachmentIds.delete(attachmentId);
        writeLocalArtifacts(state.sessionId);
      }
      renderAttachmentViews();
      renderConversation();
      return;
    }
    const save = event.target.closest("[data-save-attachment]");
    if (save) return openSaveDialog(attachmentById(save.dataset.saveAttachment));
    const toolPreview = event.target.closest("[data-preview-tool]");
    if (toolPreview) {
      const tool = state.tools.get(toolPreview.dataset.previewTool);
      if (tool?.imageDataUrl) {
        if (previewDialog) {
          $("#media-preview-title").textContent = "工具图片";
          $("#media-preview-image").src = tool.imageDataUrl;
          $("#media-preview-image").alt = "工具图片";
          $("#media-preview-meta").textContent = tool.resultText || "工具产物";
          if (!previewDialog.open) previewDialog.showModal();
        } else window.open(tool.imageDataUrl, "_blank", "noopener,noreferrer");
      }
    }
  });
}

function openSaveDialog(attachment) {
  if (!attachment || attachment.isImage) return showToast("仅支持文本附件", "warning");
  $("#workspace-save-artifact-id").value = attachment.id;
  $("#workspace-save-path").value = attachment.name;
  if (!saveDialog.open) saveDialog.showModal();
  window.setTimeout(() => $("#workspace-save-path").select(), 0);
}

async function saveAttachmentToWorkspace(attachment, path) {
  const sessionId = state.sessionId;
  const generation = state.sessionGeneration;
  const operationId = `artifact-write-${crypto.randomUUID()}`;
  const turnId = `artifact-turn-${crypto.randomUUID()}`;
  const runId = `artifact-run-${crypto.randomUUID()}`;
  const stale = () => generation !== state.sessionGeneration || state.sessionId !== sessionId;
  const base = {
    command_id: crypto.randomUUID(),
    session_id: sessionId,
    turn_id: turnId,
    run_id: runId,
    principal: "web-user",
    operation_id: operationId,
    tool_name: "write_file",
    intent: { kind: "WriteFile", data: { path, content: attachment.content } },
  };
  let result = await client.executeTool(base);
  if (stale()) return "stale";
  if (!result?.accepted) {
    await refreshSessionEvents(generation, sessionId).catch(() => undefined);
    if (stale()) return "stale";
    const tool = state.tools.get(operationId);
    if (tool?.requestDigest) {
      result = await client.executeTool({
        ...base,
        command_id: crypto.randomUUID(),
        approval: {
          request_digest: tool.requestDigest,
          actor: "web-user",
          nonce: crypto.randomUUID(),
          expires_at_ms: Date.now() + 5 * 60 * 1000,
        },
      });
      if (stale()) return "stale";
    }
  }
  if (result?.accepted) {
    if (stale()) return "stale";
    appendToolResult(operationId, result);
    await refreshSessionEvents(generation, sessionId).catch(() => undefined);
    return stale() ? "stale" : true;
  }
  return false;
}

saveForm.addEventListener("submit", async (event) => {
  event.preventDefault();
  const attachment = attachmentById($("#workspace-save-artifact-id").value);
  const path = $("#workspace-save-path").value.trim();
  if (!attachment || !path) return showToast("请输入路径", "warning");
  const submit = saveForm.querySelector('button[type="submit"]');
  submit.disabled = true;
  submit.textContent = "写入…";
  try {
    const saved = await saveAttachmentToWorkspace(attachment, path);
    if (saved === true) {
      saveDialog.close();
      showToast(`已写入：${path}`, "success");
    } else if (saved === "stale") {
      saveDialog.close();
      showToast("会话已切换", "info");
    } else showToast("等待审批", "warning");
  } catch (error) {
    showToast(displayError(error, "工作区写入失败"), "error");
  } finally {
    submit.disabled = false;
    submit.textContent = "写入";
  }
});
for (const selector of ["#workspace-save-close", "#workspace-save-cancel"]) $(selector).addEventListener("click", () => saveDialog.close());
if (previewDialog) $("#media-preview-close").addEventListener("click", () => previewDialog.close());

inspectForm.addEventListener("submit", async (event) => {
  event.preventDefault();
  const sessionId = state.sessionId;
  const generation = state.sessionGeneration;
  if (!sessionId) return showToast("先选择会话", "warning");
  const operationId = $("#operation-id").value.trim();
  if (!operationId) return showToast("请输入操作 ID", "warning");
  try {
    const result = await client.inspectOperation({ session_id: sessionId, operation_id: operationId });
    if (generation !== state.sessionGeneration || state.sessionId !== sessionId) return;
    state.lastOperationId = operationId;
    $("#operation-result").textContent = JSON.stringify(result, null, 2);
    const status = result.operation?.status;
    continueButton.disabled = !(status === "unknown" || status === "recovery_required");
    showToast(approvalForOperation(result.operation), "info");
  } catch (error) {
    if (generation === state.sessionGeneration && state.sessionId === sessionId) showToast(displayError(error), "error");
  }
});

function approvalForOperation(operation) {
  const status = operation?.status ?? "unknown";
  return OPERATION_STATUS_LABELS[status] ?? status;
}

continueButton.addEventListener("click", async () => {
  const sessionId = state.sessionId;
  const generation = state.sessionGeneration;
  const operationId = state.lastOperationId;
  if (!sessionId || !operationId) return;
  continueButton.disabled = true;
  let continuationAccepted = false;
  try {
    const result = await client.continueOperation({
      command_id: crypto.randomUUID(),
      session_id: sessionId,
      operation_id: operationId,
      principal: "web-user",
    });
    if (generation !== state.sessionGeneration || state.sessionId !== sessionId) return;
    $("#operation-result").textContent = JSON.stringify(result, null, 2);
    continuationAccepted = Boolean(result.accepted);
    showToast(result.accepted ? "已提交" : resultError(result.error, "需先检查"), result.accepted ? "success" : "warning");
  } catch (error) {
    if (generation === state.sessionGeneration && state.sessionId === sessionId) showToast(displayError(error), "error");
  } finally {
    if (generation === state.sessionGeneration && state.sessionId === sessionId) continueButton.disabled = continuationAccepted;
  }
});

toolQueue.addEventListener("click", async (event) => {
  const button = event.target.closest("[data-execute-tool]");
  const sessionId = state.sessionId;
  const generation = state.sessionGeneration;
  if (!button || !sessionId) return;
  const tool = state.tools.get(button.dataset.executeTool);
  if (!tool?.intent) return showToast("缺少工具参数", "error");
  button.disabled = true;
  try {
    const approval = tool.requestDigest
      ? { request_digest: tool.requestDigest, actor: "web-user", nonce: crypto.randomUUID(), expires_at_ms: Date.now() + 5 * 60 * 1000 }
      : undefined;
    const result = await client.executeTool({
      command_id: crypto.randomUUID(),
      session_id: sessionId,
      turn_id: tool.turnId || "web-tool-turn",
      run_id: tool.runId || "web-tool-run",
      principal: "web-user",
      operation_id: tool.operationId,
      tool_name: tool.toolName,
      intent: tool.intent,
      approval,
    });
    if (generation !== state.sessionGeneration || state.sessionId !== sessionId) return;
    appendToolResult(tool.operationId, result);
    showToast(result.accepted ? "已完成" : result.error ?? "需审批", result.accepted ? "success" : "warning");
  } catch (error) {
    if (generation === state.sessionGeneration && state.sessionId === sessionId) showToast(displayError(error, "工具执行失败，请重试。"), "error");
  } finally {
    if (generation === state.sessionGeneration && state.sessionId === sessionId) renderToolQueue();
  }
});

$("#settings-button").addEventListener("click", async () => {
  if (!settingsDialog.open) settingsDialog.showModal();
  try {
    await loadSettings();
    if (typeof client.codexAuthStatus === "function") await refreshCodexAuthStatus();
  } catch (error) {
    settingsStatus.textContent = "读取失败";
    showToast(displayError(error, "读取失败"), "error");
  }
});

for (const selector of ["#settings-close", "#settings-cancel"]) $(selector).addEventListener("click", () => settingsDialog.close());
$("#add-provider-button").addEventListener("click", addProviderFromForm);
$("#provider-kind").addEventListener("change", syncProviderEditor);
$("#discover-provider-models-button").addEventListener("click", discoverProviderModels);
$("#provider-model-catalog").addEventListener("change", applyDiscoveredModel);
$("#codex-copy-device-code").addEventListener("click", copyCodexDeviceCode);
$("#codex-login-button").addEventListener("click", () => startCodexLogin("browser"));
$("#codex-device-login-button").addEventListener("click", () => startCodexLogin("device_code"));
$("#codex-logout-button").addEventListener("click", logoutCodex);
syncProviderEditor();
renderTurnParameters();
for (const control of [thinkingEffortSelect, temperatureInput, maxOutputTokensInput]) {
  control.addEventListener("change", syncTurnParametersFromForm);
}
$("#reset-turn-parameters").addEventListener("click", () => {
  const defaults = normalizeTurnParameters();
  state.turnParameters = defaults;
  persistTurnParameters(defaults);
  renderTurnParameters(defaults);
});
modelSelect.addEventListener("change", () => {
  $("#model-pill").textContent = modelSelect.value || NO_MODEL_LABEL;
});

providerList.addEventListener("click", (event) => {
  const button = event.target.closest("[data-remove-provider]");
  if (!button || !state.settings) return;
  const providerId = button.dataset.removeProvider;
  const provider = state.settings.providers.find((item) => item.id === providerId);
  const ownsDefault = provider?.models?.some((model) => model.id === state.settings.default_model);
  if (ownsDefault) return showToast("不能删除当前默认模型所属供应商", "warning");
  state.settings = { ...state.settings, providers: state.settings.providers.filter((item) => item.id !== providerId) };
  applySettings(state.settings);
  settingsStatus.textContent = "已移除，保存后生效";
});

settingsForm.addEventListener("submit", async (event) => {
  event.preventDefault();
  try {
    await saveSettings();
  } catch (error) {
    settingsStatus.textContent = "保存失败";
    showToast(displayError(error, "保存失败"), "error");
  }
});

window.addEventListener("beforeunload", () => state.closeStream?.());

try {
  const health = await client.request("runtime.v1.health");
  setConnection(health?.status === "ok");
} catch {
  setConnection(false);
}

loadSettings().catch(() => {
  settingsStatus.textContent = "设置暂不可用";
});
loadSessions();
