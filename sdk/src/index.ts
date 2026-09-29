import * as runtime from "./runtime.mjs";

export type RpcId = string | number;

export interface JsonRpcRequest {
  jsonrpc: "2.0";
  id?: RpcId;
  method: string;
  params?: unknown;
}

export interface JsonRpcResponse<T = unknown> {
  jsonrpc: "2.0";
  id: RpcId;
  result?: T;
  error?: { code: number; message: string; data?: unknown };
}

export interface EventEnvelope {
  protocol: string;
  version: number;
  schema_version: number;
  event_id: string;
  session_id: string;
  sequence: number;
  global_sequence: number;
  recorded_at_ms: number;
  correlation_id?: string;
  causation_id?: string;
  prev_hash: string;
  hash: string;
  payload: { type: string; data?: Record<string, unknown> };
}

export interface ModelMessage {
  role: string;
  content: string;
}

export interface CompiledPrompt {
  compiler_version: string;
  messages: ModelMessage[];
  stable_prefix: string;
  dynamic_tail: ModelMessage[];
  digest: string;
}

export interface RpcTransport {
  request(request: JsonRpcRequest): Promise<JsonRpcResponse>;
  onMessage?(listener: (message: unknown) => void): () => void;
}

export const PROTOCOL_NAME = runtime.PROTOCOL_NAME;
export const PROTOCOL_VERSION = runtime.PROTOCOL_VERSION;
export const EVENT_SCHEMA_VERSION = runtime.EVENT_SCHEMA_VERSION;
export const canonicalize = runtime.canonicalize;
export const canonicalJson = runtime.canonicalJson;
export const sha256 = runtime.sha256;
export const requestDigest = runtime.requestDigest;
export const stableModelHistory = runtime.stableModelHistory;
export const JsonRpcClient = runtime.JsonRpcClient;
export const StdioJsonRpcTransport = runtime.StdioJsonRpcTransport;
export const RpcError = runtime.RpcError;
export const createInMemoryTransport = runtime.createInMemoryTransport;
export { BrowserHarnessClient, BrowserRpcError, FetchJsonRpcTransport } from "./browser.mjs";

export interface BackendInspection {
  state: string;
  external_id?: string;
  detail?: string;
  result?: unknown;
}

export interface OutboxDelivery {
  message_id: number;
  subscription_id: string;
  event: EventEnvelope;
  delivery_attempts: number;
  available_at_ms: number;
}

export interface ArtifactRecord {
  metadata: {
    artifact_id: string;
    session_id: string;
    operation_id: string;
    kind: string;
    sha256: string;
    bytes: number;
    media_type: string;
    created_at_ms: number;
  };
  content: string;
}
