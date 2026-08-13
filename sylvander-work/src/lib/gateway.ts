import { Channel, invoke } from "@tauri-apps/api/core";

export type DesktopEvent =
  | { type: "connected"; protocol: { server_name: string; version: number; capabilities: string[] } }
  | { type: "message"; message: RuntimeMessage }
  | { type: "disconnected"; reason: string };

export type RuntimeCommand =
  | { type: "discover_agents" }
  | { type: "list_sessions"; include_archived: boolean }
  | { type: "load_session"; session_id: string }
  | { type: "reattach_session"; session_id: string }
  | { type: "rename_session"; session_id: string; label: string }
  | { type: "archive_session"; session_id: string }
  | { type: "restore_session"; session_id: string }
  | { type: "fork_session"; session_id: string; completed_turns?: number; checkpoint: boolean }
  | { type: "delete_session"; session_id: string }
  | { type: "chat"; text: string; attachments: []; session_id?: string }
  | { type: "approve"; session_id: string; call_id: string; approved: boolean; scope: ApprovalScope; reason?: string }
  | { type: "interrupt"; session_id: string }
  | { type: "answer"; session_id: string; call_id: string; answer: string }
  | { type: "resolve_plan"; session_id: string; plan_id: string; decision: PlanDecision }
  | { type: "cancel_task"; session_id: string; task_id: string }
  | { type: "create_session"; request: { agent_id: string; label: string; channel_id?: string; overrides: Record<string, never> } }
  | { type: "get_runtime_info"; agent_id: string }
  | { type: "get_context"; session_id?: string }
  | { type: "compact"; session_id: string }
  | { type: "select_model"; session_id?: string; model: { provider_id: string; model_id: string }; reasoning_effort: ReasoningEffort }
  | { type: "select_permissions"; session_id?: string; profile: RuntimePermissionProfile }
  | { type: "inspect_coding_session"; session_id: string }
  | { type: "accept_coding_session"; session_id: string }
  | { type: "discard_coding_session"; session_id: string }
  | { type: "preview_workspace_rollback"; session_id: string }
  | { type: "rollback_workspace"; session_id: string; expected_turn_id: string }
  | { type: "get_session_config"; session_id: string }
  | { type: "update_session_config"; request: RuntimeSessionConfigUpdateRequest }
  | { type: "ping" };

export type PlanDecision =
  | { decision: "approved" }
  | { decision: "revised"; steps: string[] }
  | { decision: "rejected"; reason: string };

export type ApprovalScope = "once" | "session" | "persistent";
export type ReasoningEffort = "off" | "low" | "medium" | "high";

export interface RuntimePermissionProfile {
  file_access: "none" | "read_only" | "workspace_write";
  network_access: "denied" | "allowed";
  approval_policy: "ask" | "allow" | "deny";
}

export interface RuntimeModelDescriptor {
  id: string;
  provider: string;
  capabilities: number;
  capability_names: string[];
  reasoning_efforts: ReasoningEffort[];
  lifecycle: { status: "active" } | { status: "deprecated"; replacement?: string };
}

export type RuntimeConfigFieldPatch<T> =
  | { operation: "inherit" }
  | { operation: "set"; value: T };

export interface RuntimeSessionConfigPatch {
  model?: RuntimeConfigFieldPatch<{ provider_id: string; model_id: string }>;
  reasoning_effort?: RuntimeConfigFieldPatch<ReasoningEffort>;
  permissions?: RuntimeConfigFieldPatch<RuntimePermissionProfile>;
}

export interface RuntimeSessionConfigUpdateRequest {
  session_id: string;
  expected_revision: number;
  patch: RuntimeSessionConfigPatch;
}

interface RuntimeConfigSource {
  kind: "agent_default" | "channel_default" | "session_override" | "request_override";
  reference?: string;
}

export interface RuntimeSessionConfigState {
  session_id: string;
  revision: number;
  overrides: {
    model?: { provider_id: string; model_id: string };
    reasoning_effort?: ReasoningEffort;
    permissions?: RuntimePermissionProfile;
  };
  effective: {
    provider_id: string;
    model_id: string;
    reasoning_effort: ReasoningEffort;
    permissions: RuntimePermissionProfile;
    provenance: {
      model: RuntimeConfigSource;
      reasoning_effort: RuntimeConfigSource;
      permissions: RuntimeConfigSource;
    };
  };
}

export interface RuntimeSession {
  id: string;
  label: string;
  workspace: string;
  last_seen_secs: number;
  archived: boolean;
}

export interface RuntimeHistoryMessage {
  role: string;
  text: string;
}

export interface RuntimeSessionHistory {
  session: RuntimeSession;
  messages: RuntimeHistoryMessage[];
  iterations?: number;
  input_tokens?: number;
  output_tokens?: number;
  cost_nano_usd?: number;
  notice?: string;
  source_session_id?: string;
  recovery?: boolean;
  replay_truncated?: boolean;
}

export interface RuntimeAgent {
  id: string;
  revision: number;
  name: string;
  provider_id: string;
  default_model_id: string;
}

export interface RuntimeInfo {
  agent_id: string;
  model: { provider_id: string; model_id: string };
  reasoning_effort: ReasoningEffort;
  models: RuntimeModelDescriptor[];
  permissions: RuntimePermissionProfile;
  capabilities: number;
  approval_enabled: boolean;
  max_request_bytes: number;
  platform: unknown;
}

export interface RuntimeContextReport {
  model: string;
  context_window: number;
  used_tokens: number;
  remaining_tokens: number;
  cache_read_tokens: number;
  cache_write_tokens: number;
  sources: Array<{ kind: "system_prompt" | "conversation" | "tools"; label: string; items: number }>;
}

export interface RuntimeCompactionReport {
  automatic: boolean;
  removed_messages: number;
  condensed_blocks: number;
  freed_tokens: number;
  summary?: string;
}

interface RuntimeModelRetry {
  session_id: string;
  attempt: number;
  max_attempts: number;
  delay_ms: number;
  reason: string;
  cause: "rate_limit" | "server" | "network" | "stream" | "other";
}

interface RuntimeInteractionTimeout {
  session_id: string;
  kind: "approval" | "question" | "plan" | "tool" | "task";
  subject_id: string;
  timeout_secs: number;
  recovery: "retry_request" | "narrow_scope" | "continue_without";
}

interface RuntimeBoundaryError {
  code: "unauthenticated" | "forbidden" | "invalid_scope" | "payload_too_large" | "rate_limited";
  operation: string;
  request_id: string;
  message: string;
  retry_after_ms?: number;
}

export type RuntimeMessage =
  | { type: "sessions_list"; include_archived: boolean; sessions: RuntimeSession[] }
  | { type: "agents_discovered"; agents: RuntimeAgent[] }
  | { type: "runtime_info"; snapshot: RuntimeInfo }
  | ({ type: "session_history" } & RuntimeSessionHistory)
  | { type: "text_delta"; session_id: string; delta: string }
  | { type: "thinking_delta"; session_id: string; delta: string }
  | ({ type: "model_retry" } & RuntimeModelRetry)
  | ({ type: "interaction_timeout" } & RuntimeInteractionTimeout)
  | { type: "tool_call"; session_id: string; call_id: string; tool_name: string; input: unknown }
  | { type: "tool_output_delta"; session_id: string; call_id: string; tool_name: string; delta: string }
  | { type: "tool_result"; session_id: string; call_id: string; tool_name: string; output: string; is_error: boolean }
  | { type: "iteration_start"; session_id: string; iteration: number }
  | { type: "iteration_end"; session_id: string; iteration: number; input_tokens: number; output_tokens: number; cost_nano_usd?: number }
  | { type: "context_report"; report: RuntimeContextReport }
  | { type: "compaction_started"; session_id: string; automatic: boolean }
  | { type: "compaction_completed"; session_id: string; report: RuntimeCompactionReport }
  | { type: "compaction_failed"; session_id: string; automatic: boolean; reason: string }
  | { type: "coding_session_diff"; session_id: string; diff: { status: string; patch: string } }
  | { type: "coding_session_accepted"; session_id: string }
  | { type: "coding_session_discarded"; session_id: string }
  | { type: "coding_session_operation_failed"; session_id: string; operation: string; reason: string }
  | { type: "workspace_rollback_preview"; session_id: string; preview: { turn_id: string; files: string[] } }
  | { type: "workspace_rollback_completed"; session_id: string; report: { turn_id: string; restored: string[] } }
  | { type: "workspace_rollback_failed"; session_id: string; reason: string }
  | { type: "session_config"; state: RuntimeSessionConfigState }
  | { type: "pong" }
  | { type: "approval_request"; session_id: string; batch_id: string; tools: Array<{ call_id: string; tool_name: string; input: unknown }>; allowed_scopes?: ApprovalScope[] }
  | { type: "tool_rejected"; session_id: string; tool_name: string; reason: string }
  | { type: "ask_user"; session_id: string; call_id: string; question: string; options: string[]; multi_select: boolean }
  | { type: "plan_proposed" | "plan_updated"; session_id: string; plan_id: string; steps: string[]; current: number }
  | { type: "task_started"; session_id: string; task_id: string; owner: string; purpose: string }
  | { type: "task_progress"; session_id: string; task_id: string; message: string }
  | { type: "task_completed"; session_id: string; task_id: string; summary: string }
  | { type: "task_failed"; session_id: string; task_id: string; error: string }
  | { type: "task_cancelled"; session_id: string; task_id: string; reason: string }
  | { type: "done"; session_id: string; text: string }
  | { type: "error"; session_id: string; message: string }
  | { type: "turn_interrupted"; session_id: string; reason: string }
  | { type: "session_created"; session_id: string; config?: RuntimeSessionConfigState }
  | { type: "session_updated"; session_id: string; label?: string; archived: boolean }
  | { type: "session_deleted"; session_id: string }
  | { type: "operation_error"; operation: string; message: string }
  | { type: "boundary_denied"; error: RuntimeBoundaryError };

export interface RuntimeGatewayPort {
  connect(listener: (event: DesktopEvent) => void): Promise<void>;
  submit(message: RuntimeCommand): Promise<void>;
  disconnect(): Promise<void>;
}

export class RuntimeGateway implements RuntimeGatewayPort {
  async connect(listener: (event: DesktopEvent) => void): Promise<void> {
    const events = new Channel<DesktopEvent>();
    events.onmessage = listener;
    await invoke("connect_runtime", { events });
  }

  submit(message: RuntimeCommand): Promise<void> {
    return invoke("submit_runtime", { message });
  }

  disconnect(): Promise<void> {
    return invoke("disconnect_runtime");
  }
}
