import { Channel, invoke } from "@tauri-apps/api/core";

export type DesktopEvent =
  | { type: "connected"; protocol: { server_name: string; version: number; capabilities: string[] } }
  | { type: "message"; message: RuntimeMessage }
  | { type: "disconnected"; reason: string };

export type RuntimeCommand =
  | { type: "list_sessions" }
  | { type: "load_session"; session_id: string }
  | { type: "chat"; text: string; attachments: []; session_id?: string }
  | { type: "approve"; session_id: string; call_id: string; approved: boolean; scope: "once"; reason?: string }
  | { type: "interrupt"; session_id: string }
  | { type: "answer"; session_id: string; call_id: string; answer: string }
  | { type: "resolve_plan"; session_id: string; plan_id: string; decision: PlanDecision }
  | { type: "get_runtime_info" };

export type PlanDecision =
  | { decision: "approved" }
  | { decision: "revised"; steps: string[] }
  | { decision: "rejected"; reason: string };

export interface RuntimeSession {
  id: string;
  label: string;
  workspace: string;
  last_seen_secs: number;
}

export interface RuntimeHistoryMessage {
  role: string;
  text: string;
}

export type RuntimeMessage =
  | { type: "sessions_list"; sessions: RuntimeSession[] }
  | { type: "session_history"; session: RuntimeSession; messages: RuntimeHistoryMessage[] }
  | { type: "text_delta"; session_id: string; delta: string }
  | { type: "thinking_delta"; session_id: string; delta: string }
  | { type: "tool_call"; session_id: string; call_id: string; tool_name: string; input: unknown }
  | { type: "tool_result"; session_id: string; call_id: string; tool_name: string; output: string; is_error: boolean }
  | { type: "approval_request"; session_id: string; batch_id: string; tools: Array<{ call_id: string; tool_name: string; input: unknown }>; allowed_scopes?: string[] }
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
  | { type: "session_created"; session_id: string }
  | { type: "session_updated"; session_id: string; label?: string; archived: boolean }
  | { type: "session_deleted"; session_id: string };

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
