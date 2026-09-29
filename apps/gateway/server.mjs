#!/usr/bin/env node

import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { extname, join, normalize } from "node:path";
import { spawn } from "node:child_process";
import { randomUUID } from "node:crypto";
import { StdioJsonRpcTransport } from "../../sdk/src/runtime.mjs";
import { handleCodexRpc } from "./codex-auth.mjs";
import { handleProviderModelsRpc } from "./provider-models.mjs";

const root = new URL("../web/", import.meta.url);
const port = Number(process.env.HARNESS_GATEWAY_PORT ?? 8787);
const host = process.env.HARNESS_GATEWAY_HOST ?? "127.0.0.1";
const daemonCommand = process.env.HARNESSD ?? "harnessd";
const gatewayToken = process.env.HARNESS_GATEWAY_TOKEN ?? "";
const daemon = new StdioJsonRpcTransport(daemonCommand, [], {
  cwd: process.cwd(),
  env: process.env,
});
let requestId = 1;

function jsonHeaders() {
  return {
    "content-type": "application/json; charset=utf-8",
    "cache-control": "no-store",
    "x-content-type-options": "nosniff",
  };
}

function sendJson(response, status, body) {
  response.writeHead(status, jsonHeaders());
  response.end(JSON.stringify(body));
}

function requestAuthorized(request, url) {
  const loopback = ["127.0.0.1", "localhost", "::1"].includes(url.hostname)
    || host === "127.0.0.1"
    || host === "localhost"
    || host === "::1";
  if (!gatewayToken) return loopback;
  return request.headers.authorization === `Bearer ${gatewayToken}`
    || url.searchParams.get("access_token") === gatewayToken;
}

async function readBody(request, maxBytes = 2 * 1024 * 1024) {
  const chunks = [];
  let size = 0;
  for await (const chunk of request) {
    size += chunk.length;
    if (size > maxBytes) throw new Error("request body is too large");
    chunks.push(chunk);
  }
  return Buffer.concat(chunks).toString("utf8");
}

async function forwardRpc(payload) {
  const request = {
    jsonrpc: "2.0",
    id: payload.id ?? `gateway-${requestId++}`,
    method: payload.method,
    params: payload.params ?? {},
  };
  return daemon.request(request);
}

function safeAsset(pathname) {
  const requested = pathname === "/" ? "index.html" : pathname.replace(/^\/+/, "");
  const clean = normalize(requested);
  const webPath = clean.replaceAll("\\", "/");
  if (webPath.startsWith("..") || webPath.includes("../")) return null;
  if (webPath === "sdk/browser.mjs") return new URL("../../sdk/src/browser.mjs", import.meta.url);
  return new URL(webPath, root);
}

const types = {
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".mjs": "text/javascript; charset=utf-8",
  ".css": "text/css; charset=utf-8",
  ".json": "application/json; charset=utf-8",
};

async function serveAsset(response, pathname) {
  const url = safeAsset(pathname);
  if (!url) return sendJson(response, 404, { error: "not found" });
  try {
    const body = await readFile(url);
    response.writeHead(200, {
      "content-type": types[extname(url.pathname)] ?? "application/octet-stream",
      "cache-control": "no-cache",
      "x-content-type-options": "nosniff",
    });
    response.end(body);
  } catch {
    sendJson(response, 404, { error: "not found" });
  }
}

async function streamEvents(request, response, url) {
  const sessionId = url.searchParams.get("session_id") ?? "";
  const existingSubscriptionId = url.searchParams.get("subscription_id");
  const after = Number(url.searchParams.get("after_global_sequence") ?? 0);
  const subscription = await forwardRpc({
    method: "runtime.v1.events.subscribe",
    params: existingSubscriptionId
      ? { subscription_id: existingSubscriptionId, limit: 100 }
      : { session_id: sessionId, after_global_sequence: Number.isFinite(after) ? after : 0, limit: 100 },
  });
  if (subscription.error) return sendJson(response, 502, subscription);
  response.writeHead(200, {
    "content-type": "text/event-stream; charset=utf-8",
    "cache-control": "no-cache, no-transform",
    connection: "keep-alive",
    "x-accel-buffering": "no",
  });
  response.write(`: connected subscription=${subscription.result?.subscription_id ?? ""}\n\n`);
  const sent = new Set();
  const send = (delivery) => {
    const event = delivery?.event ?? delivery;
    if (!event || (sessionId && event.session_id !== sessionId) || sent.has(event.event_id)) return;
    sent.add(event.event_id);
    response.write(`id: ${event.global_sequence ?? ""}\nevent: session/event\ndata: ${JSON.stringify(event)}\n\n`);
  };
  for (const delivery of subscription.result?.deliveries ?? []) send(delivery);
  const subscriptionId = subscription.result?.subscription_id;
  let closed = false;
  const heartbeat = setInterval(() => {
    if (!closed) response.write(`: heartbeat ${Date.now()}\n\n`);
  }, 15_000);
  const poll = setInterval(async () => {
    if (closed || !subscriptionId) return;
    try {
      const result = await forwardRpc({
        method: "runtime.v1.events.pull",
        params: { subscription_id: subscriptionId, limit: 100, lease_ms: 60_000 },
      });
      for (const delivery of result.result?.deliveries ?? []) send(delivery);
    } catch {
      if (!closed) response.write(`event: gateway/error\ndata: {"message":"event pull failed"}\n\n`);
    }
  }, 1_000);
  request.on("close", () => {
    closed = true;
    clearInterval(heartbeat);
    clearInterval(poll);
  });
}

const server = createServer(async (request, response) => {
  try {
    const url = new URL(request.url ?? "/", `http://${request.headers.host ?? `${host}:${port}`}`);
    if (["/rpc", "/events"].includes(url.pathname) && !requestAuthorized(request, url)) {
      return sendJson(response, 401, { error: "gateway authentication required" });
    }
    if (request.method === "OPTIONS") {
      response.writeHead(204, { "access-control-allow-methods": "GET,POST,OPTIONS", "access-control-allow-headers": "content-type" });
      return response.end();
    }
    if (request.method === "GET" && url.pathname === "/health") {
      return sendJson(response, 200, { status: "ok", service: "harness-gateway" });
    }
    if (request.method === "POST" && url.pathname === "/rpc") {
      const payload = JSON.parse(await readBody(request));
      const localCodexResult = await handleCodexRpc(payload);
      const localProviderModelsResult = await handleProviderModelsRpc(payload);
      const result = localCodexResult ?? localProviderModelsResult ?? await forwardRpc(payload);
      response.writeHead(200, jsonHeaders());
      return response.end(JSON.stringify(result));
    }
    if (request.method === "GET" && url.pathname === "/events") {
      return streamEvents(request, response, url);
    }
    if (request.method === "GET") return serveAsset(response, url.pathname);
    sendJson(response, 405, { error: "method not allowed" });
  } catch (error) {
    sendJson(response, 400, { error: error instanceof Error ? error.message : String(error) });
  }
});

server.listen(port, host, () => {
  process.stdout.write(`harness gateway listening on http://${host}:${port}\n`);
});

function shutdown() {
  server.close();
  daemon.close();
}
process.on("SIGINT", shutdown);
process.on("SIGTERM", shutdown);
