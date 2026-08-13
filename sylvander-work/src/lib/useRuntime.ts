import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import { RuntimeGateway, type DesktopEvent, type RuntimeCommand, type RuntimeGatewayPort, type RuntimeMessage } from "./gateway";
import type { ConnectionState, PlanStep, SessionSummary, TaskSummary, TranscriptEntry } from "./types";

export interface RuntimeViewState {
  connection: ConnectionState;
  sessions: SessionSummary[];
  selectedId?: string;
  transcript: TranscriptEntry[];
  plan: PlanStep[];
  tasks: TaskSummary[];
  approval?: { sessionId: string; callId: string; toolName: string };
  diagnostic?: string;
}

const initialState: RuntimeViewState = {
  connection: "starting",
  sessions: [],
  transcript: [],
  plan: [],
  tasks: [],
};

export function useRuntime(injectedGateway?: RuntimeGatewayPort) {
  const nativeGateway = useMemo(() => new RuntimeGateway(), []);
  const gateway = injectedGateway ?? nativeGateway;
  const [state, setState] = useState(initialState);
  const selectedRef = useRef<string | undefined>(undefined);
  const pendingDeltasRef = useRef(new Map<string, string>());
  const frameRef = useRef<number | undefined>(undefined);

  const submit = useCallback((message: RuntimeCommand) => gateway.submit(message), [gateway]);

  const enqueueDelta = useCallback((sessionId: string, delta: string) => {
    const pending = pendingDeltasRef.current;
    pending.set(sessionId, (pending.get(sessionId) ?? "") + delta);
    if (frameRef.current !== undefined) return;

    frameRef.current = requestAnimationFrame(() => {
      frameRef.current = undefined;
      const selectedId = selectedRef.current;
      const combined = selectedId ? pending.get(selectedId) : undefined;
      pending.clear();
      if (combined) {
        setState((current) => ({ ...current, transcript: appendDelta(current.transcript, combined) }));
      }
    });
  }, []);

  const applyMessage = useCallback((message: RuntimeMessage) => {
    switch (message.type) {
      case "sessions_list": {
        const sessions = message.sessions.map((session) => ({
          id: session.id,
          label: session.label,
          workspace: session.workspace,
          recency: formatRecency(session.last_seen_secs),
          state: "idle" as const,
          draft: "",
        }));
        setState((current) => ({ ...current, sessions }));
        if (!selectedRef.current && sessions[0]) {
          selectedRef.current = sessions[0].id;
          setState((current) => ({ ...current, selectedId: sessions[0].id }));
          void submit({ type: "load_session", session_id: sessions[0].id });
        }
        break;
      }
      case "session_history":
        if (message.session.id !== selectedRef.current) break;
        setState((current) => ({
          ...current,
          transcript: message.messages.map((item, index) => ({
            id: `history-${index}`,
            kind: item.role === "user" ? "user" : "assistant",
            body: item.text,
          })),
        }));
        break;
      case "text_delta":
        enqueueDelta(message.session_id, message.delta);
        break;
      case "tool_call":
        if (message.session_id === selectedRef.current) {
          setState((current) => ({ ...current, transcript: [...current.transcript, {
            id: `tool-${message.call_id}`,
            kind: "tool",
            title: message.tool_name,
            body: "Running through Runtime",
            status: "running",
          }] }));
        }
        break;
      case "tool_result":
        if (message.session_id === selectedRef.current) {
          setState((current) => ({ ...current, transcript: current.transcript.map((entry) =>
            entry.id === `tool-${message.call_id}`
              ? { ...entry, body: message.output, status: message.is_error ? "failed" : "verified" }
              : entry) }));
        }
        break;
      case "approval_request": {
        const tool = message.tools[0];
        if (tool) setState((current) => ({ ...current, approval: { sessionId: message.session_id, callId: tool.call_id, toolName: tool.tool_name } }));
        break;
      }
      case "plan_proposed":
      case "plan_updated":
        if (message.session_id === selectedRef.current) {
          setState((current) => ({ ...current, plan: message.steps.map((label, index) => ({
            label,
            state: index < message.current ? "complete" : index === message.current ? "current" : "pending",
          })) }));
        }
        break;
      case "task_started":
        if (message.session_id === selectedRef.current) {
          setState((current) => ({ ...current, tasks: [...current.tasks, { owner: message.owner, purpose: message.purpose, state: "running" }] }));
        }
        break;
      case "done":
      case "error":
      case "turn_interrupted":
        if (message.session_id === selectedRef.current) {
          setState((current) => ({ ...current, sessions: current.sessions.map((session) =>
            session.id === message.session_id ? { ...session, state: message.type === "error" ? "failed" : "idle" } : session) }));
        }
        break;
      default:
        break;
    }
  }, [enqueueDelta, submit]);

  const onEvent = useCallback((event: DesktopEvent) => {
    if (event.type === "connected") {
      setState((current) => ({ ...current, connection: "live", diagnostic: undefined }));
      void submit({ type: "list_sessions" });
      void submit({ type: "get_runtime_info" });
    } else if (event.type === "message") {
      applyMessage(event.message);
    } else {
      setState((current) => ({ ...current, connection: "offline", diagnostic: event.reason }));
    }
  }, [applyMessage, submit]);

  useEffect(() => {
    setState((current) => ({ ...current, connection: "connecting" }));
    gateway.connect(onEvent).catch((error: unknown) => {
      setState((current) => ({ ...current, connection: "offline", diagnostic: safeDiagnostic(error) }));
    });
    return () => { void gateway.disconnect().catch(() => undefined); };
  }, [gateway, onEvent]);

  useEffect(() => () => {
    if (frameRef.current !== undefined) cancelAnimationFrame(frameRef.current);
  }, []);

  const selectSession = useCallback((sessionId: string) => {
    selectedRef.current = sessionId;
    setState((current) => ({ ...current, selectedId: sessionId, transcript: [], plan: [], tasks: [] }));
    return submit({ type: "load_session", session_id: sessionId });
  }, [submit]);

  return { state, submit, selectSession };
}

function appendDelta(entries: TranscriptEntry[], delta: string): TranscriptEntry[] {
  const last = entries.at(-1);
  if (last?.id === "streaming-assistant") {
    return [...entries.slice(0, -1), { ...last, body: last.body + delta }];
  }
  return [...entries, { id: "streaming-assistant", kind: "assistant", body: delta }];
}

function formatRecency(seconds: number): string {
  if (seconds < 60) return "Now";
  if (seconds < 3600) return `${Math.floor(seconds / 60)} min`;
  if (seconds < 86400) return `${Math.floor(seconds / 3600)} hr`;
  return `${Math.floor(seconds / 86400)} d`;
}

function safeDiagnostic(error: unknown): string {
  return typeof error === "string" && error.trim()
    ? error
    : "The native Runtime gateway is unavailable. Check the Runtime endpoint and try again.";
}
