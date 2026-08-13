import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import { RuntimeGateway, type DesktopEvent, type PlanDecision, type RuntimeCommand, type RuntimeGatewayPort, type RuntimeMessage } from "./gateway";
import type { ConnectionState, PlanStep, SessionSummary, TaskSummary, TranscriptEntry } from "./types";

export interface RuntimeViewState {
  connection: ConnectionState;
  sessions: SessionSummary[];
  selectedId?: string;
  transcript: TranscriptEntry[];
  plan: PlanStep[];
  activePlan?: { sessionId: string; planId: string };
  tasks: TaskSummary[];
  approval?: {
    sessionId: string;
    batchId: string;
    tools: Array<{ callId: string; toolName: string }>;
  };
  question?: {
    sessionId: string;
    callId: string;
    prompt: string;
    options: string[];
    multiSelect: boolean;
  };
  diagnostic?: string;
}

const initialState: RuntimeViewState = {
  connection: "starting",
  sessions: [],
  transcript: [],
  plan: [],
  tasks: [],
};

// Retry scheduling is presentation orchestration only. Each attempt still
// crosses the native gateway, which alone owns credentials and negotiation.
const RECONNECT_BASE_MS = 1_000;
const RECONNECT_MAX_MS = 10_000;

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
          setState((current) => ({
            ...current,
            approval: settleApproval(current.approval, message.call_id),
            transcript: [...current.transcript, {
              id: `tool-${message.call_id}`,
              kind: "tool",
              title: message.tool_name,
              body: "Running through Runtime",
              status: "running",
            }],
          }));
        }
        break;
      case "tool_result":
        if (message.session_id === selectedRef.current) {
          setState((current) => ({
            ...current,
            approval: settleApproval(current.approval, message.call_id),
            transcript: current.transcript.map((entry) => entry.id === `tool-${message.call_id}`
              ? { ...entry, body: message.output, status: message.is_error ? "failed" : "verified" }
              : entry),
          }));
        }
        break;
      case "approval_request":
        if (message.session_id === selectedRef.current && message.tools.length > 0) {
          setState((current) => ({ ...current, approval: {
            sessionId: message.session_id,
            batchId: message.batch_id,
            tools: message.tools.map((tool) => ({ callId: tool.call_id, toolName: tool.tool_name })),
          } }));
        }
        break;
      case "tool_rejected":
        if (message.session_id === selectedRef.current) {
          setState((current) => ({
            ...current,
            approval: rejectApproval(current.approval, message.tool_name),
          }));
        }
        break;
      case "ask_user":
        if (message.session_id === selectedRef.current) {
          setState((current) => ({ ...current, question: {
            sessionId: message.session_id,
            callId: message.call_id,
            prompt: message.question,
            options: message.options,
            multiSelect: message.multi_select,
          } }));
        }
        break;
      case "plan_proposed":
      case "plan_updated":
        if (message.session_id === selectedRef.current) {
          setState((current) => ({
            ...current,
            activePlan: { sessionId: message.session_id, planId: message.plan_id },
            plan: message.steps.map((label, index) => ({
              label,
              state: index < message.current ? "complete" : index === message.current ? "current" : "pending",
            })),
          }));
        }
        break;
      case "task_started":
        if (message.session_id === selectedRef.current) {
          setState((current) => ({ ...current, tasks: upsertTask(current.tasks, {
            id: message.task_id,
            owner: message.owner,
            purpose: message.purpose,
            state: "running",
          }) }));
        }
        break;
      case "task_progress":
        if (message.session_id === selectedRef.current) {
          setState((current) => ({ ...current, tasks: updateTask(current.tasks, message.task_id, {
            detail: message.message,
            state: "running",
          }) }));
        }
        break;
      case "task_completed":
        if (message.session_id === selectedRef.current) {
          setState((current) => ({ ...current, tasks: updateTask(current.tasks, message.task_id, {
            detail: message.summary,
            state: "complete",
          }) }));
        }
        break;
      case "task_failed":
        if (message.session_id === selectedRef.current) {
          setState((current) => ({ ...current, tasks: updateTask(current.tasks, message.task_id, {
            detail: message.error,
            state: "failed",
          }) }));
        }
        break;
      case "task_cancelled":
        if (message.session_id === selectedRef.current) {
          setState((current) => ({ ...current, tasks: updateTask(current.tasks, message.task_id, {
            detail: message.reason,
            state: "cancelled",
          }) }));
        }
        break;
      case "done":
      case "error":
      case "turn_interrupted":
        if (message.session_id === selectedRef.current) {
          setState((current) => ({
            ...current,
            approval: undefined,
            question: undefined,
            activePlan: undefined,
            sessions: current.sessions.map((session) => session.id === message.session_id
              ? { ...session, state: message.type === "error" ? "failed" : "idle" }
              : session),
          }));
        }
        break;
      default:
        break;
    }
  }, [enqueueDelta, submit]);

  useEffect(() => {
    let stopped = false;
    let connectedOnce = false;
    let reconnectAttempt = 0;
    let reconnectTimer: number | undefined;

    const scheduleReconnect = (reason: string) => {
      if (stopped || reconnectTimer !== undefined) return;
      const delay = Math.min(RECONNECT_BASE_MS * 2 ** reconnectAttempt, RECONNECT_MAX_MS);
      reconnectAttempt += 1;
      setState((current) => ({
        ...current,
        connection: connectedOnce ? "reconnecting" : "offline",
        diagnostic: reason,
      }));
      reconnectTimer = window.setTimeout(() => {
        reconnectTimer = undefined;
        void connect();
      }, delay);
    };

    const onEvent = (event: DesktopEvent) => {
      if (stopped) return;
      if (event.type === "connected") {
        if (reconnectTimer !== undefined) {
          window.clearTimeout(reconnectTimer);
          reconnectTimer = undefined;
        }
        connectedOnce = true;
        reconnectAttempt = 0;
        setState((current) => ({ ...current, connection: "live", diagnostic: undefined }));
        void submit({ type: "list_sessions" });
        void submit({ type: "get_runtime_info" });
      } else if (event.type === "message") {
        applyMessage(event.message);
      } else {
        scheduleReconnect(event.reason);
      }
    };

    const connect = async () => {
      if (stopped) return;
      if (connectedOnce) {
        setState((current) => ({ ...current, connection: "reconnecting" }));
      }
      try {
        await gateway.connect(onEvent);
      } catch (error: unknown) {
        scheduleReconnect(safeDiagnostic(error));
      }
    };

    setState((current) => ({ ...current, connection: "connecting" }));
    void connect();
    return () => {
      stopped = true;
      if (reconnectTimer !== undefined) window.clearTimeout(reconnectTimer);
      void gateway.disconnect().catch(() => undefined);
    };
  }, [applyMessage, gateway, submit]);

  useEffect(() => () => {
    if (frameRef.current !== undefined) cancelAnimationFrame(frameRef.current);
  }, []);

  const selectSession = useCallback((sessionId: string) => {
    selectedRef.current = sessionId;
    setState((current) => ({
      ...current,
      selectedId: sessionId,
      transcript: [],
      plan: [],
      activePlan: undefined,
      tasks: [],
      approval: undefined,
      question: undefined,
    }));
    return submit({ type: "load_session", session_id: sessionId });
  }, [submit]);

  const answerQuestion = useCallback(async (callId: string, answer: string) => {
    const question = state.question;
    if (!question || question.callId !== callId) return;
    await submit({
      type: "answer",
      session_id: question.sessionId,
      call_id: question.callId,
      answer,
    });
    setState((current) => current.question?.callId === callId
      ? { ...current, question: undefined }
      : current);
  }, [state.question, submit]);

  const resolvePlan = useCallback(async (
    planId: string,
    decision: PlanDecision,
  ) => {
    const plan = state.activePlan;
    if (!plan || plan.planId !== planId) return;
    await submit({
      type: "resolve_plan",
      session_id: plan.sessionId,
      plan_id: plan.planId,
      decision,
    });
    setState((current) => current.activePlan?.planId === planId
      ? { ...current, activePlan: undefined }
      : current);
  }, [state.activePlan, submit]);

  return { state, submit, selectSession, answerQuestion, resolvePlan };
}

function appendDelta(entries: TranscriptEntry[], delta: string): TranscriptEntry[] {
  const last = entries.at(-1);
  if (last?.id === "streaming-assistant") {
    return [...entries.slice(0, -1), { ...last, body: last.body + delta }];
  }
  return [...entries, { id: "streaming-assistant", kind: "assistant", body: delta }];
}

/**
 * Project one Runtime task identity into presentation state.
 *
 * Reattach replay may repeat a start event, so task identity—not arrival
 * count—defines the row. Runtime remains authoritative; this helper never
 * creates a durable task or invents a terminal transition.
 */
function upsertTask(tasks: TaskSummary[], task: TaskSummary): TaskSummary[] {
  const index = tasks.findIndex((candidate) => candidate.id === task.id);
  if (index < 0) return [...tasks, task];
  return tasks.map((candidate, candidateIndex) => candidateIndex === index ? task : candidate);
}

function updateTask(
  tasks: TaskSummary[],
  taskId: string,
  update: Pick<TaskSummary, "detail" | "state">,
): TaskSummary[] {
  return tasks.map((task) => task.id === taskId ? { ...task, ...update } : task);
}

function settleApproval(
  approval: RuntimeViewState["approval"],
  callId: string,
): RuntimeViewState["approval"] {
  if (!approval?.tools.some((tool) => tool.callId === callId)) return approval;
  const tools = approval.tools.filter((tool) => tool.callId !== callId);
  return tools.length > 0 ? { ...approval, tools } : undefined;
}

function rejectApproval(
  approval: RuntimeViewState["approval"],
  toolName: string,
): RuntimeViewState["approval"] {
  const callId = approval?.tools.find((tool) => tool.toolName === toolName)?.callId;
  return callId ? settleApproval(approval, callId) : approval;
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
