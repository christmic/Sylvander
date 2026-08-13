import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import { RuntimeGateway, type ApprovalScope, type DesktopEvent, type PlanDecision, type ReasoningEffort, type RuntimeAgentAdminRequest, type RuntimeAgentRevisionView, type RuntimeCommand, type RuntimeCompactionReport, type RuntimeContextReport, type RuntimeCredentialGenerationView, type RuntimeGatewayPort, type RuntimeIdentityBindingAction, type RuntimeIdentityBindingView, type RuntimeMessage, type RuntimeModelDescriptor, type RuntimeModelRevisionView, type RuntimePendingMemoryConfirmation, type RuntimeProviderRevisionView, type RuntimeRegistryAdminRequest, type RuntimeSessionConfigState, type RuntimeUserProfileAction, type RuntimeUserProfileExport, type RuntimeUserProfileOperation, type RuntimeUserProfileView } from "./gateway";
import type { ConnectionState, PlanStep, SessionSummary, TaskSummary, TranscriptEntry } from "./types";

export interface RuntimeViewState {
  connection: ConnectionState;
  protocol?: { serverName: string; version: number; capabilities: string[] };
  runtimeInfo?: {
    providerId: string;
    modelId: string;
    reasoningEffort: ReasoningEffort;
    models: RuntimeModelDescriptor[];
    fileAccess: "none" | "read_only" | "workspace_write";
    networkAccess: "denied" | "allowed";
    approvalPolicy: "ask" | "allow" | "deny";
    approvalEnabled: boolean;
  };
  agents: Array<{ id: string; name: string; providerId: string; modelId: string }>;
  sessions: SessionSummary[];
  archivedSessions: SessionSummary[];
  selectedId?: string;
  sessionStats?: {
    iterations: number;
    inputTokens: number;
    outputTokens: number;
    costNanoUsd?: number;
    sourceSessionId?: string;
  };
  transcript: TranscriptEntry[];
  plan: PlanStep[];
  activePlan?: { sessionId: string; planId: string };
  tasks: TaskSummary[];
  approval?: {
    sessionId: string;
    batchId: string;
    tools: Array<{ callId: string; toolName: string }>;
    allowedScopes: ApprovalScope[];
  };
  question?: {
    sessionId: string;
    callId: string;
    prompt: string;
    options: string[];
    multiSelect: boolean;
  };
  interruptingSessionIds: string[];
  contextReport?: RuntimeContextReport;
  contextRequestPending: boolean;
  compaction?: {
    status: "running" | "completed" | "failed";
    automatic: boolean;
    report?: RuntimeCompactionReport;
    reason?: string;
  };
  codingReview?: {
    status: string;
    patch: string;
    outcome?: "accepted" | "failed";
    detail?: string;
  };
  rollback?: {
    turnId?: string;
    files: string[];
    status: "preview" | "completed" | "failed";
    detail?: string;
  };
  sessionConfig?: RuntimeSessionConfigState;
  feedback?: {
    target: string;
    status: "ready" | "submitting" | "recorded";
    feedbackId?: string;
  };
  memoryConfirmations: RuntimePendingMemoryConfirmation[];
  memoryDecisionPending?: string;
  userProfile: {
    status: "idle" | "loading" | "ready" | "not_found" | "submitting" | "error";
    profile?: RuntimeUserProfileView;
    export?: RuntimeUserProfileExport;
    pendingOperation?: RuntimeUserProfileOperation;
    notice?: string;
  };
  identityBinding: {
    status: "idle" | "loading" | "ready" | "not_linked" | "submitting" | "error";
    binding?: RuntimeIdentityBindingView;
    challenge?: { id: string; secret: string; expiresAtUnixSecs: number };
    pendingOperation?: RuntimeIdentityBindingAction["operation"];
    notice?: string;
  };
  agentAdministration: {
    status: "idle" | "loading" | "ready" | "submitting" | "error";
    agentId?: string;
    activeRevision?: number;
    revisions: RuntimeAgentRevisionView[];
    nextBeforeRevision?: number;
    pendingOperation?: RuntimeAgentAdminRequest["operation"];
    notice?: string;
  };
  registryAdministration: {
    status: "idle" | "loading" | "ready" | "submitting" | "error";
    pendingOperation?: RuntimeRegistryAdminRequest["operation"];
    provider?: { id: string; activeRevision: number; revisions: RuntimeProviderRevisionView[]; nextBeforeRevision?: number };
    model?: { providerId: string; modelId: string; activeRevision: number; revisions: RuntimeModelRevisionView[]; nextBeforeRevision?: number };
    credential?: { bindingId: string; bindingIdSha256?: string; activeGeneration: number; generations: RuntimeCredentialGenerationView[]; nextBeforeGeneration?: number };
    notice?: string;
  };
  liveness: "idle" | "checking" | "healthy";
  diagnostic?: string;
}

const initialState: RuntimeViewState = {
  connection: "starting",
  agents: [],
  sessions: [],
  archivedSessions: [],
  transcript: [],
  plan: [],
  tasks: [],
  interruptingSessionIds: [],
  contextRequestPending: false,
  memoryConfirmations: [],
  userProfile: { status: "idle" },
  identityBinding: { status: "idle" },
  agentAdministration: { status: "idle", revisions: [] },
  registryAdministration: { status: "idle" },
  liveness: "idle",
};

// Retry scheduling is presentation orchestration only. Each attempt still
// crosses the native gateway, which alone owns credentials and negotiation.
const RECONNECT_BASE_MS = 1_000;
const RECONNECT_MAX_MS = 10_000;
const RUNNING_TOOL_PLACEHOLDER = "Running through Runtime";

type PendingDelta =
  | { kind: "assistant" | "thinking"; delta: string }
  | { kind: "tool"; callId: string; delta: string };

export function useRuntime(injectedGateway?: RuntimeGatewayPort) {
  const nativeGateway = useMemo(() => new RuntimeGateway(), []);
  const gateway = injectedGateway ?? nativeGateway;
  const [state, setState] = useState(initialState);
  const selectedRef = useRef<string | undefined>(undefined);
  const pendingDeltasRef = useRef(new Map<string, PendingDelta[]>());
  const frameRef = useRef<number | undefined>(undefined);
  const terminalSequenceRef = useRef(0);
  const localTurnStateRef = useRef(new Map<string, "waiting" | "active">());
  const interruptingSessionsRef = useRef(new Set<string>());
  const contextRequestSessionRef = useRef<string | undefined>(undefined);
  const protocolCapabilitiesRef = useRef(new Set<string>());
  const memoryRequestSessionRef = useRef<string | undefined>(undefined);
  const userProfileRequestRef = useRef<RuntimeUserProfileOperation | undefined>(undefined);
  const identityBindingRequestRef = useRef<RuntimeIdentityBindingAction["operation"] | undefined>(undefined);
  const agentAdminRequestRef = useRef<RuntimeAgentAdminRequest["operation"] | undefined>(undefined);
  const registryAdminRequestRef = useRef<RuntimeRegistryAdminRequest | undefined>(undefined);

  const submit = useCallback((message: RuntimeCommand) => gateway.submit(message), [gateway]);

  // Memory candidates are Runtime-owned. Desktop only asks for the selected
  // Session's latest bounded projection after capability negotiation.
  const requestMemoryConfirmations = useCallback((sessionId: string) => {
    if (!protocolCapabilitiesRef.current.has("memory_confirmation_v1")) return;
    memoryRequestSessionRef.current = sessionId;
    void submit({
      type: "memory_confirmation",
      request: { operation: "list", version: 1, session_id: sessionId },
    }).catch((error: unknown) => {
      if (memoryRequestSessionRef.current !== sessionId) return;
      memoryRequestSessionRef.current = undefined;
      setState((current) => ({ ...current, diagnostic: safeDiagnostic(error) }));
    });
  }, [submit]);

  // Profile contents are sensitive owner data. They remain in this ephemeral
  // projection and are never copied into Session history or diagnostics.
  const requestUserProfile = useCallback(async (action: RuntimeUserProfileAction) => {
    if (!protocolCapabilitiesRef.current.has("user_profile_v1")) return false;
    userProfileRequestRef.current = action.operation;
    setState((current) => ({
      ...current,
      userProfile: {
        ...current.userProfile,
        status: action.operation === "read" ? "loading" : "submitting",
        profile: action.operation === "read" ? undefined : current.userProfile.profile,
        pendingOperation: action.operation,
        export: undefined,
        notice: undefined,
      },
    }));
    try {
      await submit({ type: "user_profile", request: { version: 1, action } });
      return true;
    } catch {
      if (userProfileRequestRef.current === action.operation) {
        userProfileRequestRef.current = undefined;
      }
      setState((current) => ({
        ...current,
        userProfile: {
          ...current.userProfile,
          status: "error",
          pendingOperation: undefined,
          notice: "Runtime command queue is unavailable",
        },
      }));
      return false;
    }
  }, [submit]);

  // Challenge proofs are bearer secrets. Keep them out of generic diagnostics
  // and discard the entire projection when the dedicated surface closes.
  const requestIdentityBinding = useCallback(async (action: RuntimeIdentityBindingAction) => {
    if (!protocolCapabilitiesRef.current.has("identity_binding_v1")) return false;
    identityBindingRequestRef.current = action.operation;
    setState((current) => ({
      ...current,
      identityBinding: {
        ...current.identityBinding,
        status: action.operation === "resolve" ? "loading" : "submitting",
        challenge: undefined,
        pendingOperation: action.operation,
        notice: undefined,
      },
    }));
    try {
      await submit({ type: "identity_binding", request: { version: 1, action } });
      return true;
    } catch {
      if (identityBindingRequestRef.current === action.operation) {
        identityBindingRequestRef.current = undefined;
      }
      setState((current) => ({
        ...current,
        identityBinding: {
          ...current.identityBinding,
          status: "error",
          challenge: undefined,
          pendingOperation: undefined,
          notice: "Runtime identity command queue is unavailable",
        },
      }));
      return false;
    }
  }, [submit]);

  const requestAgentAdministration = useCallback(async (request: RuntimeAgentAdminRequest) => {
    if (!protocolCapabilitiesRef.current.has("agent_administration")) return false;
    agentAdminRequestRef.current = request.operation;
    setState((current) => ({
      ...current,
      agentAdministration: {
        ...current.agentAdministration,
        status: request.operation === "list_revisions" || request.operation === "inspect_revision"
          ? "loading"
          : "submitting",
        pendingOperation: request.operation,
        notice: undefined,
      },
    }));
    try {
      await submit({ type: "agent_admin", request });
      return true;
    } catch {
      if (agentAdminRequestRef.current === request.operation) agentAdminRequestRef.current = undefined;
      setState((current) => ({
        ...current,
        agentAdministration: {
          ...current.agentAdministration,
          status: "error",
          pendingOperation: undefined,
          notice: "Runtime administration command queue is unavailable",
        },
      }));
      return false;
    }
  }, [submit]);

  const requestRegistryAdministration = useCallback(async (request: RuntimeRegistryAdminRequest) => {
    if (!protocolCapabilitiesRef.current.has("registry_administration")) return false;
    registryAdminRequestRef.current = request;
    setState((current) => ({
      ...current,
      registryAdministration: {
        ...current.registryAdministration,
        status: registryReadOperation(request.operation) ? "loading" : "submitting",
        pendingOperation: request.operation,
        notice: undefined,
      },
    }));
    try {
      await submit({ type: "registry_admin", request });
      return true;
    } catch {
      if (registryAdminRequestRef.current === request) registryAdminRequestRef.current = undefined;
      setState((current) => ({
        ...current,
        registryAdministration: {
          ...current.registryAdministration,
          status: "error",
          pendingOperation: undefined,
          notice: "Runtime registry command queue is unavailable",
        },
      }));
      return false;
    }
  }, [submit]);

  const flushPendingDeltas = useCallback((sessionId: string) => {
    const pending = pendingDeltasRef.current;
    const combined = pending.get(sessionId);
    pending.delete(sessionId);
    if (sessionId !== selectedRef.current || !combined || combined.length === 0) return;
    setState((current) => ({
      ...current,
      transcript: combined.reduce(applyPendingDelta, current.transcript),
    }));
  }, []);

  const applyTurnStarted = useCallback((sessionId: string) => {
    if (localTurnStateRef.current.get(sessionId) === "active") return;
    localTurnStateRef.current.set(sessionId, "active");
    if (selectedRef.current === sessionId) memoryRequestSessionRef.current = undefined;
    setState((current) => ({
      ...current,
      sessions: current.sessions.map((session) => session.id === sessionId
        ? { ...session, state: "active" }
        : session),
      feedback: current.selectedId === sessionId ? undefined : current.feedback,
      memoryConfirmations: current.selectedId === sessionId ? [] : current.memoryConfirmations,
      memoryDecisionPending: current.selectedId === sessionId
        ? undefined
        : current.memoryDecisionPending,
    }));
  }, []);

  const enqueueDelta = useCallback((sessionId: string, delta: PendingDelta) => {
    const pending = pendingDeltasRef.current;
    const events = pending.get(sessionId) ?? [];
    const last = events.at(-1);
    if (last && sameDeltaTarget(last, delta)) {
      last.delta += delta.delta;
    } else {
      events.push(delta);
    }
    pending.set(sessionId, events);
    if (frameRef.current !== undefined) return;

    frameRef.current = requestAnimationFrame(() => {
      frameRef.current = undefined;
      const selectedId = selectedRef.current;
      if (selectedId) flushPendingDeltas(selectedId);
      pending.clear();
    });
  }, [flushPendingDeltas]);

  const applyMessage = useCallback((message: RuntimeMessage) => {
    switch (message.type) {
      case "agents_discovered": {
        const agents = message.agents.map((agent) => ({
          id: agent.id,
          name: agent.name,
          providerId: agent.provider_id,
          modelId: agent.default_model_id,
        }));
        setState((current) => ({
          ...current,
          agents,
        }));
        if (agents[0]) void submit({ type: "get_runtime_info", agent_id: agents[0].id });
        break;
      }
      case "runtime_info": {
        const snapshot = message.snapshot;
        setState((current) => ({
          ...current,
          runtimeInfo: {
            providerId: snapshot.model.provider_id,
            modelId: snapshot.model.model_id,
            reasoningEffort: snapshot.reasoning_effort,
            models: snapshot.models,
            fileAccess: snapshot.permissions.file_access,
            networkAccess: snapshot.permissions.network_access,
            approvalPolicy: snapshot.permissions.approval_policy,
            approvalEnabled: snapshot.approval_enabled,
          },
        }));
        break;
      }
      case "turn_started":
        applyTurnStarted(message.session_id);
        break;
      case "iteration_start":
        break;
      case "iteration_end":
        if (message.session_id === selectedRef.current) {
          setState((current) => ({
            ...current,
            sessionStats: {
              ...current.sessionStats,
              iterations: (current.sessionStats?.iterations ?? 0) + 1,
              inputTokens: message.input_tokens,
              outputTokens: message.output_tokens,
              costNanoUsd: message.cost_nano_usd,
            },
          }));
        }
        break;
      case "context_report":
        if (!contextRequestSessionRef.current
          || contextRequestSessionRef.current !== selectedRef.current) break;
        contextRequestSessionRef.current = undefined;
        setState((current) => ({
          ...current,
          contextReport: message.report,
          contextRequestPending: false,
        }));
        break;
      case "compaction_started":
        if (message.session_id === selectedRef.current) {
          setState((current) => ({
            ...current,
            compaction: { status: "running", automatic: message.automatic },
          }));
        }
        break;
      case "compaction_completed":
        if (message.session_id === selectedRef.current) {
          setState((current) => ({
            ...current,
            compaction: {
              status: "completed",
              automatic: message.report.automatic,
              report: message.report,
            },
          }));
        }
        break;
      case "compaction_failed":
        if (message.session_id === selectedRef.current) {
          setState((current) => ({
            ...current,
            compaction: {
              status: "failed",
              automatic: message.automatic,
              reason: message.reason,
            },
          }));
        }
        break;
      case "coding_session_diff":
        if (message.session_id === selectedRef.current) {
          setState((current) => ({ ...current, codingReview: message.diff }));
        }
        break;
      case "coding_session_accepted":
        if (message.session_id === selectedRef.current) {
          setState((current) => ({
            ...current,
            codingReview: { status: "", patch: "", outcome: "accepted" },
          }));
        }
        break;
      case "coding_session_discarded":
        localTurnStateRef.current.delete(message.session_id);
        interruptingSessionsRef.current.delete(message.session_id);
        if (selectedRef.current === message.session_id) {
          selectedRef.current = undefined;
          contextRequestSessionRef.current = undefined;
        }
        setState((current) => ({
          ...current,
          selectedId: current.selectedId === message.session_id ? undefined : current.selectedId,
          sessions: current.sessions.filter((session) => session.id !== message.session_id),
          interruptingSessionIds: current.interruptingSessionIds.filter(
            (sessionId) => sessionId !== message.session_id,
          ),
          transcript: current.selectedId === message.session_id ? [] : current.transcript,
          plan: current.selectedId === message.session_id ? [] : current.plan,
          activePlan: current.selectedId === message.session_id ? undefined : current.activePlan,
          tasks: current.selectedId === message.session_id ? [] : current.tasks,
          sessionStats: current.selectedId === message.session_id ? undefined : current.sessionStats,
          sessionConfig: current.selectedId === message.session_id ? undefined : current.sessionConfig,
          contextReport: current.selectedId === message.session_id ? undefined : current.contextReport,
          contextRequestPending: current.selectedId === message.session_id
            ? false
            : current.contextRequestPending,
          compaction: current.selectedId === message.session_id ? undefined : current.compaction,
          codingReview: current.selectedId === message.session_id ? undefined : current.codingReview,
          rollback: current.selectedId === message.session_id ? undefined : current.rollback,
          approval: current.selectedId === message.session_id ? undefined : current.approval,
          question: current.selectedId === message.session_id ? undefined : current.question,
        }));
        break;
      case "coding_session_operation_failed":
        if (message.session_id === selectedRef.current) {
          setState((current) => ({
            ...current,
            codingReview: {
              status: current.codingReview?.status ?? "",
              patch: current.codingReview?.patch ?? "",
              outcome: "failed",
              detail: `${message.operation}: ${message.reason}`,
            },
          }));
        }
        break;
      case "workspace_rollback_preview":
        if (message.session_id === selectedRef.current) {
          setState((current) => ({
            ...current,
            rollback: {
              turnId: message.preview.turn_id,
              files: message.preview.files,
              status: "preview",
            },
          }));
        }
        break;
      case "workspace_rollback_completed":
        if (message.session_id === selectedRef.current) {
          setState((current) => ({
            ...current,
            rollback: {
              turnId: message.report.turn_id,
              files: message.report.restored,
              status: "completed",
            },
          }));
        }
        break;
      case "workspace_rollback_failed":
        if (message.session_id === selectedRef.current) {
          setState((current) => ({
            ...current,
            rollback: {
              ...current.rollback,
              files: current.rollback?.files ?? [],
              status: "failed",
              detail: message.reason,
            },
          }));
        }
        break;
      case "session_config":
        if (message.state.session_id === selectedRef.current) {
          setState((current) => ({ ...current, sessionConfig: message.state }));
        }
        break;
      case "pong":
        setState((current) => ({ ...current, liveness: "healthy" }));
        break;
      case "feedback_recorded":
        setState((current) => current.feedback?.status === "submitting" ? {
          ...current,
          feedback: { ...current.feedback, status: "recorded", feedbackId: message.feedback_id },
        } : current);
        break;
      case "memory_confirmation": {
        const response = message.response;
        if (response.result === "pending") {
          if (response.session_id !== selectedRef.current) break;
          memoryRequestSessionRef.current = undefined;
          setState((current) => ({
            ...current,
            memoryConfirmations: response.confirmations,
            memoryDecisionPending: undefined,
          }));
          break;
        }
        if (response.result === "recorded") {
          if (response.session_id !== selectedRef.current) break;
          memoryRequestSessionRef.current = undefined;
          setState((current) => current.memoryDecisionPending === response.candidate_id
            ? {
                ...current,
                memoryConfirmations: current.memoryConfirmations.filter(
                  (candidate) => candidate.candidate_id !== response.candidate_id,
                ),
                memoryDecisionPending: undefined,
              }
            : current);
          break;
        }
        const requestSessionId = memoryRequestSessionRef.current;
        memoryRequestSessionRef.current = undefined;
        if (!requestSessionId || requestSessionId !== selectedRef.current) break;
        terminalSequenceRef.current += 1;
        setState((current) => ({
          ...current,
          memoryDecisionPending: undefined,
          transcript: [...current.transcript, {
            id: `notice-memory-${terminalSequenceRef.current}`,
            kind: "notice",
            body: `memory confirmation failed · ${response.message}`,
            status: "failed",
          }],
        }));
        if (response.code === "conflict") requestMemoryConfirmations(requestSessionId);
        break;
      }
      case "user_profile": {
        const response = message.response;
        const pendingOperation = userProfileRequestRef.current;
        if (!pendingOperation) break;
        const responseOperation = profileResponseOperation(response);
        if (responseOperation && responseOperation !== pendingOperation) break;
        userProfileRequestRef.current = undefined;
        if (response.result === "not_found") {
          setState((current) => ({
            ...current,
            userProfile: { status: "not_found", notice: "No user profile is stored" },
          }));
          break;
        }
        if (response.result === "error") {
          if (response.error.code === "conflict") {
            void requestUserProfile({ operation: "read" });
            setState((current) => ({
              ...current,
              userProfile: {
                ...current.userProfile,
                notice: "Profile changed elsewhere; the stale edit was not applied",
              },
            }));
            break;
          }
          setState((current) => ({
            ...current,
            userProfile: {
              ...current.userProfile,
              status: "error",
              pendingOperation: undefined,
              notice: profileErrorNotice(response.error.code, response.error.retry_after_ms),
            },
          }));
          break;
        }
        if (response.result === "deleted") {
          setState((current) => ({
            ...current,
            userProfile: {
              status: "not_found",
              notice: response.do_not_learn_preserved
                ? "Profile deleted; do-not-learn remains enabled"
                : "Profile deleted",
            },
          }));
          break;
        }
        if (response.result === "exported") {
          setState((current) => ({
            ...current,
            userProfile: {
              status: "ready",
              profile: response.export.profile,
              export: response.export,
              notice: "Profile export is ready",
            },
          }));
          break;
        }
        setState((current) => ({
          ...current,
          userProfile: {
            status: "ready",
            profile: response.profile,
            notice: profileSuccessNotice(response.result),
          },
        }));
        break;
      }
      case "identity_binding": {
        const pendingOperation = identityBindingRequestRef.current;
        const response = message.response;
        if (!pendingOperation || !identityResponseMatches(pendingOperation, response)) break;
        identityBindingRequestRef.current = undefined;
        if (response.result === "challenge_issued") {
          setState((current) => ({
            ...current,
            identityBinding: {
              status: "ready",
              challenge: {
                id: response.challenge_id,
                secret: response.secret,
                expiresAtUnixSecs: response.expires_at_unix_secs,
              },
              notice: "One-time link proof issued",
            },
          }));
          break;
        }
        if (response.result === "resolved") {
          setState((current) => ({
            ...current,
            identityBinding: {
              status: "ready",
              binding: response.binding,
              notice: "Identity binding resolved",
            },
          }));
          break;
        }
        if (response.result === "not_linked" || response.result === "unlinked") {
          setState((current) => ({
            ...current,
            identityBinding: {
              status: "not_linked",
              notice: response.result === "unlinked" ? "Identity binding removed" : "No identity binding exists",
            },
          }));
          break;
        }
        setState((current) => ({
          ...current,
          identityBinding: {
            ...current.identityBinding,
            status: "error",
            challenge: undefined,
            pendingOperation: undefined,
            notice: identityErrorNotice(response.error.message, response.error.retry_after_ms),
          },
        }));
        break;
      }
      case "agent_admin": {
        const pendingOperation = agentAdminRequestRef.current;
        if (!pendingOperation) break;
        const response = message.response;
        if (response.status === "error") {
          agentAdminRequestRef.current = undefined;
          const agentId = response.error.agent_id;
          if (response.error.code === "revision_conflict" && agentId) {
            void requestAgentAdministration({
              operation: "list_revisions",
              agent_id: agentId,
              limit: 50,
            });
          }
          setState((current) => ({
            ...current,
            agentAdministration: {
              ...current.agentAdministration,
              status: response.error.code === "revision_conflict" && agentId
                ? "loading"
                : "error",
              pendingOperation: response.error.code === "revision_conflict" && agentId
                ? "list_revisions"
                : undefined,
              notice: response.error.message,
            },
          }));
          break;
        }
        if (!agentAdminResponseMatches(pendingOperation, response.result.operation)) break;
        agentAdminRequestRef.current = undefined;
        if (response.result.operation === "revisions_listed") {
          const result = response.result;
          setState((current) => ({
            ...current,
            agentAdministration: {
              status: "ready",
              agentId: result.agent_id,
              activeRevision: result.active_revision,
              revisions: result.revisions,
              nextBeforeRevision: result.next_before_revision,
            },
          }));
          break;
        }
        if (response.result.operation === "revision_inspected") {
          const result = response.result;
          setState((current) => ({
            ...current,
            agentAdministration: {
              ...current.agentAdministration,
              status: "ready",
              pendingOperation: undefined,
              revisions: replaceAgentRevision(
                current.agentAdministration.revisions,
                result.revision,
              ),
            },
          }));
          break;
        }
        if (response.result.operation === "definition_updated") {
          const result = response.result;
          setState((current) => ({
            ...current,
            agentAdministration: {
              ...current.agentAdministration,
              status: "ready",
              pendingOperation: undefined,
              notice: "Agent definition staged; activation is still required",
              revisions: replaceAgentRevision(
                current.agentAdministration.revisions,
                result.revision,
              ),
            },
          }));
          break;
        }
        const result = response.result;
        setState((current) => ({
          ...current,
          agentAdministration: {
            ...current.agentAdministration,
            status: "ready",
            activeRevision: result.active_revision,
            pendingOperation: undefined,
            notice: result.operation === "revision_activated"
              ? "Agent revision activated"
              : "Agent revision rolled back",
            revisions: current.agentAdministration.revisions.map((revision) => ({
              ...revision,
              active: revision.definition.revision === result.active_revision,
            })),
          },
        }));
        break;
      }
      case "registry_admin": {
        const request = registryAdminRequestRef.current;
        const response = message.response;
        if (!request) break;
        if (response.status === "error") {
          registryAdminRequestRef.current = undefined;
          const reload = registryConflict(response.error.code)
            ? registryReloadRequest(request)
            : undefined;
          if (reload) void requestRegistryAdministration(reload);
          setState((current) => ({
            ...current,
            registryAdministration: {
              ...current.registryAdministration,
              status: reload ? "loading" : "error",
              pendingOperation: reload?.operation,
              notice: response.error.message,
            },
          }));
          break;
        }
        if (!registryResponseMatches(request.operation, response.result.operation)) break;
        registryAdminRequestRef.current = undefined;
        const result = response.result;
        if (result.operation === "provider_revisions_listed") {
          setState((current) => ({ ...current, registryAdministration: {
            ...current.registryAdministration,
            status: "ready",
            pendingOperation: undefined,
            provider: { id: result.provider_id, activeRevision: result.active_revision, revisions: result.revisions, nextBeforeRevision: result.next_before_revision },
          } }));
          break;
        }
        if (result.operation === "model_revisions_listed") {
          setState((current) => ({ ...current, registryAdministration: {
            ...current.registryAdministration,
            status: "ready",
            pendingOperation: undefined,
            model: { providerId: result.provider_id, modelId: result.model_id, activeRevision: result.active_revision, revisions: result.revisions, nextBeforeRevision: result.next_before_revision },
          } }));
          break;
        }
        if (result.operation === "credential_generations_listed") {
          const bindingId = registryBindingId(request);
          if (!bindingId) break;
          setState((current) => ({ ...current, registryAdministration: {
            ...current.registryAdministration,
            status: "ready",
            pendingOperation: undefined,
            credential: { bindingId, bindingIdSha256: result.binding_id_sha256, activeGeneration: result.active_generation, generations: result.generations, nextBeforeGeneration: result.next_before_generation },
          } }));
          break;
        }
        setState((current) => applyRegistryMutation(current, request, result));
        break;
      }
      case "sessions_list": {
        const sessions: SessionSummary[] = [];
        const archivedSessions: SessionSummary[] = [];
        for (const session of message.sessions) {
          const summary: SessionSummary = {
            id: session.id,
            label: session.label,
            workspace: session.workspace,
            recency: formatRecency(session.last_seen_secs),
            state: "idle",
            draft: "",
          };
          (session.archived ? archivedSessions : sessions).push(summary);
        }
        setState((current) => ({
          ...current,
          sessions,
          archivedSessions: message.include_archived
            ? archivedSessions
            : current.archivedSessions,
        }));
        if (!selectedRef.current && sessions[0]) {
          selectedRef.current = sessions[0].id;
          setState((current) => ({ ...current, selectedId: sessions[0].id }));
          void submit({ type: "load_session", session_id: sessions[0].id });
        }
        break;
      }
      case "session_history": {
        const selectedId = selectedRef.current;
        const isSelected = message.session.id === selectedId;
        const isFork = message.source_session_id === selectedId
          && message.session.id !== selectedId;
        if (!isSelected && !isFork) break;
        if (isFork) {
          selectedRef.current = message.session.id;
          contextRequestSessionRef.current = undefined;
        }
        setState((current) => ({
          ...current,
          selectedId: isFork ? message.session.id : current.selectedId,
          sessions: isFork && !current.sessions.some((session) => session.id === message.session.id)
            ? [{
              id: message.session.id,
              label: message.session.label,
              workspace: message.session.workspace,
              recency: formatRecency(message.session.last_seen_secs),
              state: "idle" as const,
              draft: "",
            }, ...current.sessions]
            : current.sessions,
          sessionStats: {
            iterations: message.iterations ?? 0,
            inputTokens: message.input_tokens ?? 0,
            outputTokens: message.output_tokens ?? 0,
            costNanoUsd: message.cost_nano_usd,
            sourceSessionId: message.source_session_id,
          },
          feedback: undefined,
          memoryConfirmations: [],
          memoryDecisionPending: undefined,
          transcript: [
            ...(message.notice ? [{
              id: "history-notice",
              kind: "notice" as const,
              body: message.notice,
              status: message.replay_truncated ? "failed" as const : undefined,
            }] : []),
            ...message.messages.map((item, index) => ({
            id: `history-${index}`,
            kind: item.role === "user" ? "user" as const : "assistant" as const,
            body: item.text,
            })),
          ],
        }));
        if (isFork) void submit({ type: "list_sessions", include_archived: false });
        requestMemoryConfirmations(message.session.id);
        break;
      }
      case "session_created":
        selectedRef.current = message.session_id;
        contextRequestSessionRef.current = undefined;
        setState((current) => ({
          ...current,
          selectedId: message.session_id,
          transcript: [],
          plan: [],
          tasks: [],
          sessionStats: undefined,
          sessionConfig: message.config,
          contextReport: undefined,
          contextRequestPending: false,
          compaction: undefined,
          codingReview: undefined,
          rollback: undefined,
          feedback: undefined,
          memoryConfirmations: [],
          memoryDecisionPending: undefined,
        }));
        void submit({ type: "list_sessions", include_archived: false });
        void submit({ type: "load_session", session_id: message.session_id });
        break;
      case "session_updated":
        if (message.archived) {
          if (selectedRef.current === message.session_id) {
            selectedRef.current = undefined;
            contextRequestSessionRef.current = undefined;
          }
          setState((current) => ({
            ...current,
            selectedId: current.selectedId === message.session_id ? undefined : current.selectedId,
            sessions: current.sessions.filter((session) => session.id !== message.session_id),
            transcript: current.selectedId === message.session_id ? [] : current.transcript,
            plan: current.selectedId === message.session_id ? [] : current.plan,
            tasks: current.selectedId === message.session_id ? [] : current.tasks,
            sessionStats: current.selectedId === message.session_id ? undefined : current.sessionStats,
            sessionConfig: current.selectedId === message.session_id ? undefined : current.sessionConfig,
            contextReport: current.selectedId === message.session_id ? undefined : current.contextReport,
            contextRequestPending: current.selectedId === message.session_id
              ? false
              : current.contextRequestPending,
            compaction: current.selectedId === message.session_id ? undefined : current.compaction,
            codingReview: current.selectedId === message.session_id ? undefined : current.codingReview,
            rollback: current.selectedId === message.session_id ? undefined : current.rollback,
          }));
        } else {
          setState((current) => ({
            ...current,
            archivedSessions: current.archivedSessions.filter(
              (session) => session.id !== message.session_id,
            ),
            sessions: current.sessions.map((session) => session.id === message.session_id
              ? { ...session, label: message.label ?? session.label }
              : session),
          }));
        }
        void submit({ type: "list_sessions", include_archived: false });
        void submit({ type: "list_sessions", include_archived: true });
        break;
      case "session_deleted":
        if (selectedRef.current === message.session_id) {
          selectedRef.current = undefined;
          contextRequestSessionRef.current = undefined;
        }
        setState((current) => ({
          ...current,
          selectedId: current.selectedId === message.session_id ? undefined : current.selectedId,
          sessions: current.sessions.filter((session) => session.id !== message.session_id),
          transcript: current.selectedId === message.session_id ? [] : current.transcript,
          plan: current.selectedId === message.session_id ? [] : current.plan,
          tasks: current.selectedId === message.session_id ? [] : current.tasks,
          sessionStats: current.selectedId === message.session_id ? undefined : current.sessionStats,
          sessionConfig: current.selectedId === message.session_id ? undefined : current.sessionConfig,
          contextReport: current.selectedId === message.session_id ? undefined : current.contextReport,
          contextRequestPending: current.selectedId === message.session_id
            ? false
            : current.contextRequestPending,
          compaction: current.selectedId === message.session_id ? undefined : current.compaction,
          codingReview: current.selectedId === message.session_id ? undefined : current.codingReview,
          rollback: current.selectedId === message.session_id ? undefined : current.rollback,
        }));
        break;
      case "operation_error":
        if (message.operation === "get_context") {
          contextRequestSessionRef.current = undefined;
        }
        if (message.operation === "chat" && selectedRef.current) {
          localTurnStateRef.current.delete(selectedRef.current);
        }
        terminalSequenceRef.current += 1;
        setState((current) => ({
          ...current,
          sessions: message.operation === "chat"
            ? current.sessions.map((session) => session.id === current.selectedId
              ? { ...session, state: "idle" }
              : session)
            : current.sessions,
          contextRequestPending: message.operation === "get_context"
            ? false
            : current.contextRequestPending,
          diagnostic: `${message.operation}: ${message.message}`,
          transcript: [...current.transcript, {
            id: `notice-operation-${terminalSequenceRef.current}`,
            kind: "notice",
            body: `${message.operation} failed · ${message.message}`,
            status: "failed",
          }],
        }));
        break;
      case "boundary_denied": {
        if (message.error.operation === "chat" && selectedRef.current) {
          localTurnStateRef.current.delete(selectedRef.current);
        }
        terminalSequenceRef.current += 1;
        const retry = message.error.retry_after_ms === undefined
          ? ""
          : ` · retry after ${message.error.retry_after_ms}ms`;
        setState((current) => ({
          ...current,
          sessions: message.error.operation === "chat"
            ? current.sessions.map((session) => session.id === current.selectedId
              ? { ...session, state: "idle" }
              : session)
            : current.sessions,
          diagnostic: `${message.error.operation}: ${message.error.message}`,
          transcript: [...current.transcript, {
            id: `notice-boundary-${terminalSequenceRef.current}`,
            kind: "notice",
            body: `${message.error.operation} denied · ${message.error.message}${retry}`,
            status: "failed",
          }],
        }));
        break;
      }
      case "text_delta":
        enqueueDelta(message.session_id, { kind: "assistant", delta: message.delta });
        break;
      case "thinking_delta":
        enqueueDelta(message.session_id, { kind: "thinking", delta: message.delta });
        break;
      case "model_retry":
        if (message.session_id === selectedRef.current) {
          terminalSequenceRef.current += 1;
          const sequence = terminalSequenceRef.current;
          setState((current) => ({
            ...current,
            transcript: [...current.transcript, {
              id: `notice-retry-${sequence}`,
              kind: "notice",
              body: `${retryCauseLabel(message.cause)} · retry ${message.attempt}/${message.max_attempts} in ${message.delay_ms}ms · ${message.reason}`,
              status: "waiting",
            }],
          }));
        }
        break;
      case "interaction_timeout":
        if (message.session_id === selectedRef.current) {
          terminalSequenceRef.current += 1;
          const sequence = terminalSequenceRef.current;
          setState((current) => ({
            ...current,
            approval: message.kind === "approval" ? undefined : current.approval,
            question: message.kind === "question" ? undefined : current.question,
            activePlan: message.kind === "plan" ? undefined : current.activePlan,
            transcript: [...current.transcript, {
              id: `notice-timeout-${sequence}`,
              kind: "notice",
              body: `timeout · ${timeoutKindLabel(message.kind)} · ${message.subject_id.slice(0, 8)} · ${message.timeout_secs}s · ${timeoutRecoveryLabel(message.recovery)}`,
              status: "failed",
            }],
          }));
        }
        break;
      case "tool_output_delta":
        enqueueDelta(message.session_id, {
          kind: "tool",
          callId: message.call_id,
          delta: message.delta,
        });
        break;
      case "tool_call":
        if (message.session_id === selectedRef.current) {
          flushPendingDeltas(message.session_id);
          setState((current) => ({
            ...current,
            approval: settleApproval(current.approval, message.call_id),
            transcript: [...current.transcript, {
              id: `tool-${message.call_id}`,
              kind: "tool",
              title: message.tool_name,
              body: RUNNING_TOOL_PLACEHOLDER,
              status: "running",
            }],
          }));
        }
        break;
      case "tool_result":
        if (message.session_id === selectedRef.current) {
          flushPendingDeltas(message.session_id);
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
            allowedScopes: message.allowed_scopes?.length ? message.allowed_scopes : ["once"],
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
        localTurnStateRef.current.delete(message.session_id);
        interruptingSessionsRef.current.delete(message.session_id);
        if (message.session_id === selectedRef.current) {
          flushPendingDeltas(message.session_id);
          terminalSequenceRef.current += 1;
          const sequence = terminalSequenceRef.current;
          const finalText = message.type === "done" ? message.text : undefined;
          const notice = message.type === "error"
            ? message.message
            : message.type === "turn_interrupted" ? message.reason : undefined;
          setState((current) => ({
            ...current,
            interruptingSessionIds: current.interruptingSessionIds.filter(
              (sessionId) => sessionId !== message.session_id,
            ),
            approval: undefined,
            question: undefined,
            activePlan: undefined,
            feedback: message.feedback_target
              ? { target: message.feedback_target, status: "ready" }
              : undefined,
            transcript: settleTurnTranscript(
              current.transcript,
              sequence,
              finalText,
              notice,
            ),
            sessions: current.sessions.map((session) => session.id === message.session_id
              ? { ...session, state: message.type === "error" ? "failed" : "idle" }
              : session),
          }));
          requestMemoryConfirmations(message.session_id);
        } else {
          setState((current) => ({
            ...current,
            interruptingSessionIds: current.interruptingSessionIds.filter(
              (sessionId) => sessionId !== message.session_id,
            ),
            sessions: current.sessions.map((session) => session.id === message.session_id
              ? { ...session, state: message.type === "error" ? "failed" : "idle" }
              : session),
          }));
        }
        break;
      default:
        break;
    }
  }, [applyTurnStarted, enqueueDelta, flushPendingDeltas, requestAgentAdministration, requestIdentityBinding, requestMemoryConfirmations, requestRegistryAdministration, requestUserProfile, submit]);

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
        const reconnected = connectedOnce;
        connectedOnce = true;
        reconnectAttempt = 0;
        protocolCapabilitiesRef.current = new Set(event.protocol.capabilities);
        setState((current) => ({
          ...current,
          connection: "live",
          diagnostic: undefined,
          protocol: {
            serverName: event.protocol.server_name,
            version: event.protocol.version,
            capabilities: event.protocol.capabilities,
          },
        }));
        void submit({ type: "discover_agents" });
        void submit({ type: "list_sessions", include_archived: false });
        const selectedId = selectedRef.current;
        if (reconnected && selectedId) {
          void submit({ type: "reattach_session", session_id: selectedId });
        }
      } else if (event.type === "message") {
        applyMessage(event.message);
      } else {
        protocolCapabilitiesRef.current.clear();
        memoryRequestSessionRef.current = undefined;
        userProfileRequestRef.current = undefined;
        identityBindingRequestRef.current = undefined;
        agentAdminRequestRef.current = undefined;
        registryAdminRequestRef.current = undefined;
        setState((current) => ({
          ...current,
          userProfile: { status: "idle" },
          identityBinding: { status: "idle" },
          agentAdministration: { status: "idle", revisions: [] },
          registryAdministration: { status: "idle" },
        }));
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
    contextRequestSessionRef.current = undefined;
    memoryRequestSessionRef.current = undefined;
    setState((current) => ({
      ...current,
      selectedId: sessionId,
      transcript: [],
      sessionStats: undefined,
      sessionConfig: undefined,
      plan: [],
      activePlan: undefined,
      tasks: [],
      contextReport: undefined,
      contextRequestPending: false,
      compaction: undefined,
      codingReview: undefined,
      rollback: undefined,
      approval: undefined,
      question: undefined,
      feedback: undefined,
      memoryConfirmations: [],
      memoryDecisionPending: undefined,
    }));
    return submit({ type: "load_session", session_id: sessionId });
  }, [submit]);

  const clearUserProfile = useCallback(() => {
    userProfileRequestRef.current = undefined;
    setState((current) => ({ ...current, userProfile: { status: "idle" } }));
  }, []);

  const clearIdentityBinding = useCallback(() => {
    identityBindingRequestRef.current = undefined;
    setState((current) => ({ ...current, identityBinding: { status: "idle" } }));
  }, []);

  const clearIdentityChallenge = useCallback(() => {
    setState((current) => ({
      ...current,
      identityBinding: { ...current.identityBinding, challenge: undefined },
    }));
  }, []);

  const clearAgentAdministration = useCallback(() => {
    agentAdminRequestRef.current = undefined;
    setState((current) => ({
      ...current,
      agentAdministration: { status: "idle", revisions: [] },
    }));
  }, []);

  const clearRegistryAdministration = useCallback(() => {
    registryAdminRequestRef.current = undefined;
    setState((current) => ({ ...current, registryAdministration: { status: "idle" } }));
  }, []);

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

  const cancelTask = useCallback((taskId: string) => {
    const sessionId = selectedRef.current;
    const running = state.tasks.some((task) => task.id === taskId && task.state === "running");
    if (!sessionId || !running) return Promise.resolve();
    return submit({ type: "cancel_task", session_id: sessionId, task_id: taskId });
  }, [state.tasks, submit]);

  const submitFeedback = useCallback(async (
    rating: "positive" | "negative",
    note?: string,
  ) => {
    const feedback = state.feedback;
    if (!feedback || feedback.status !== "ready") return false;
    setState((current) => current.feedback?.target === feedback.target
      ? { ...current, feedback: { ...current.feedback, status: "submitting" } }
      : current);
    try {
      await submit({
        type: "submit_feedback",
        feedback: {
          target: feedback.target,
          rating,
          ...(note?.trim() ? { note: note.trim() } : {}),
          tags: [],
          artifacts: [],
          validations: [],
          privacy_class: "private",
        },
      });
      return true;
    } catch (error) {
      setState((current) => current.feedback?.target === feedback.target
        ? {
            ...current,
            feedback: { ...current.feedback, status: "ready" },
            diagnostic: safeDiagnostic(error),
          }
        : current);
      return false;
    }
  }, [state.feedback, submit]);

  const resolveMemoryConfirmation = useCallback(async (
    candidateId: string,
    decision: "confirm" | "reject",
  ) => {
    const sessionId = selectedRef.current;
    const candidate = state.memoryConfirmations.find(
      (confirmation) => confirmation.candidate_id === candidateId,
    );
    if (!sessionId || !candidate || state.memoryDecisionPending) return false;
    // Keep the candidate visible until Runtime records this exact revision.
    memoryRequestSessionRef.current = sessionId;
    setState((current) => ({ ...current, memoryDecisionPending: candidateId }));
    try {
      await submit({
        type: "memory_confirmation",
        request: {
          operation: "decide",
          version: 1,
          session_id: sessionId,
          candidate_id: candidate.candidate_id,
          expected_revision: candidate.expected_revision,
          decision,
        },
      });
      return true;
    } catch (error) {
      if (memoryRequestSessionRef.current === sessionId) {
        memoryRequestSessionRef.current = undefined;
      }
      setState((current) => ({
        ...current,
        memoryDecisionPending: undefined,
        diagnostic: safeDiagnostic(error),
      }));
      return false;
    }
  }, [state.memoryConfirmations, state.memoryDecisionPending, submit]);

  const sendChat = useCallback(async (sessionId: string, text: string) => {
    if (localTurnStateRef.current.has(sessionId)) return false;
    localTurnStateRef.current.set(sessionId, "waiting");
    setState((current) => ({
      ...current,
      sessions: current.sessions.map((session) => session.id === sessionId
        ? { ...session, state: "waiting" }
        : session),
    }));
    try {
      await submit({ type: "chat", text, attachments: [], session_id: sessionId });
      return true;
    } catch (error) {
      localTurnStateRef.current.delete(sessionId);
      terminalSequenceRef.current += 1;
      const noticeId = `notice-submit-${terminalSequenceRef.current}`;
      setState((current) => ({
        ...current,
        sessions: current.sessions.map((session) => session.id === sessionId
          ? { ...session, state: "idle" }
          : session),
        transcript: [...current.transcript, {
          id: noticeId,
          kind: "notice",
          body: safeDiagnostic(error),
          status: "failed",
        }],
      }));
      return false;
    }
  }, [submit]);

  const interruptTurn = useCallback(async (sessionId: string) => {
    const session = state.sessions.find((candidate) => candidate.id === sessionId);
    if (!session || !["active", "waiting"].includes(session.state)
      || interruptingSessionsRef.current.has(sessionId)) return;
    interruptingSessionsRef.current.add(sessionId);
    setState((current) => ({
      ...current,
      interruptingSessionIds: [...current.interruptingSessionIds, sessionId],
    }));
    try {
      await submit({ type: "interrupt", session_id: sessionId });
    } catch (error) {
      interruptingSessionsRef.current.delete(sessionId);
      setState((current) => ({
        ...current,
        interruptingSessionIds: current.interruptingSessionIds.filter(
          (candidate) => candidate !== sessionId,
        ),
        diagnostic: safeDiagnostic(error),
      }));
    }
  }, [state.sessions, submit]);

  const requestContext = useCallback(async (sessionId: string) => {
    if (contextRequestSessionRef.current) return;
    contextRequestSessionRef.current = sessionId;
    setState((current) => ({ ...current, contextRequestPending: true }));
    try {
      await submit({ type: "get_context", session_id: sessionId });
    } catch (error) {
      contextRequestSessionRef.current = undefined;
      setState((current) => ({
        ...current,
        contextRequestPending: false,
        diagnostic: safeDiagnostic(error),
      }));
    }
  }, [submit]);

  const compactContext = useCallback((sessionId: string) => {
    if (state.compaction?.status === "running") return Promise.resolve();
    return submit({ type: "compact", session_id: sessionId });
  }, [state.compaction?.status, submit]);

  const checkLiveness = useCallback(async () => {
    if (state.liveness === "checking") return;
    setState((current) => ({ ...current, liveness: "checking" }));
    try {
      await submit({ type: "ping" });
    } catch (error) {
      setState((current) => ({
        ...current,
        liveness: "idle",
        diagnostic: safeDiagnostic(error),
      }));
    }
  }, [state.liveness, submit]);

  return {
    state,
    submit,
    selectSession,
    answerQuestion,
    resolvePlan,
    cancelTask,
    submitFeedback,
    resolveMemoryConfirmation,
    requestUserProfile,
    clearUserProfile,
    requestIdentityBinding,
    clearIdentityBinding,
    clearIdentityChallenge,
    requestAgentAdministration,
    clearAgentAdministration,
    requestRegistryAdministration,
    clearRegistryAdministration,
    sendChat,
    interruptTurn,
    requestContext,
    compactContext,
    checkLiveness,
  };
}

function profileSuccessNotice(result: "created" | "read" | "updated" | "corrected" | "do_not_learn_updated") {
  switch (result) {
    case "created": return "User profile created";
    case "read": return "User profile loaded";
    case "updated": return "User profile updated";
    case "corrected": return "User profile corrected";
    case "do_not_learn_updated": return "Learning preference updated";
  }
}

function profileErrorNotice(code: string, retryAfterMs?: number) {
  const retry = retryAfterMs === undefined ? "" : `; retry in ${retryAfterMs} ms`;
  return `User profile operation failed (${code})${retry}`;
}

function profileResponseOperation(response: Extract<RuntimeMessage, { type: "user_profile" }>["response"]): RuntimeUserProfileOperation | undefined {
  switch (response.result) {
    case "created": return "create";
    case "read": return "read";
    case "updated": return "update";
    case "exported": return "export";
    case "corrected": return "correct";
    case "deleted": return "delete";
    case "do_not_learn_updated": return "set_do_not_learn";
    case "error": return response.error.operation;
    case "not_found": return undefined;
  }
}

function identityResponseMatches(
  operation: RuntimeIdentityBindingAction["operation"],
  response: Extract<RuntimeMessage, { type: "identity_binding" }>["response"],
) {
  if (response.result === "error") return response.error.operation === operation;
  if (response.result === "challenge_issued") return operation === "begin";
  if (response.result === "resolved") return operation === "confirm" || operation === "resolve";
  if (response.result === "not_linked") return operation === "resolve";
  return operation === "unlink";
}

function identityErrorNotice(message: string, retryAfterMs?: number) {
  const retry = retryAfterMs === undefined ? "" : `; retry in ${retryAfterMs} ms`;
  return `${message}${retry}`;
}

function agentAdminResponseMatches(
  request: RuntimeAgentAdminRequest["operation"],
  response: "revision_inspected" | "revisions_listed" | "definition_updated" | "revision_activated" | "revision_rolled_back",
) {
  return (request === "inspect_revision" && response === "revision_inspected")
    || (request === "list_revisions" && response === "revisions_listed")
    || (request === "update_definition" && response === "definition_updated")
    || (request === "activate_revision" && response === "revision_activated")
    || (request === "rollback_revision" && response === "revision_rolled_back");
}

function replaceAgentRevision(
  revisions: RuntimeAgentRevisionView[],
  replacement: RuntimeAgentRevisionView,
) {
  const exists = revisions.some((revision) =>
    revision.definition.revision === replacement.definition.revision);
  return exists
    ? revisions.map((revision) => revision.definition.revision === replacement.definition.revision
      ? replacement
      : revision)
    : [...revisions, replacement];
}

type RegistrySuccessResult = Extract<
  Extract<RuntimeMessage, { type: "registry_admin" }>["response"],
  { status: "success" }
>["result"];

function registryReadOperation(operation: RuntimeRegistryAdminRequest["operation"]) {
  return operation.startsWith("inspect_") || operation.startsWith("list_");
}

function registryConflict(code: string) {
  return code === "active_revision_conflict" || code === "active_generation_conflict";
}

function registryReloadRequest(request: RuntimeRegistryAdminRequest): RuntimeRegistryAdminRequest | undefined {
  switch (request.operation) {
    case "create_provider":
    case "inspect_provider_revision":
    case "list_provider_revisions": return undefined;
    case "stage_provider_revision":
    case "activate_provider_revision":
    case "rollback_provider_revision": return { operation: "list_provider_revisions", provider_id: request.provider_id, limit: 50 };
    case "create_model":
    case "inspect_model_revision":
    case "list_model_revisions": return undefined;
    case "stage_model_revision":
    case "activate_model_revision":
    case "rollback_model_revision": return { operation: "list_model_revisions", provider_id: request.provider_id, model_id: request.model_id, limit: 50 };
    case "create_credential_binding":
    case "inspect_credential_generation":
    case "list_credential_generations": return undefined;
    case "stage_credential_generation":
    case "activate_credential_generation":
    case "rollback_credential_generation": return { operation: "list_credential_generations", binding_id: request.binding_id, limit: 50 };
  }
}

function registryResponseMatches(
  request: RuntimeRegistryAdminRequest["operation"],
  response: RegistrySuccessResult["operation"],
) {
  const expected: Record<RuntimeRegistryAdminRequest["operation"], RegistrySuccessResult["operation"]> = {
    inspect_provider_revision: "provider_revision_inspected",
    list_provider_revisions: "provider_revisions_listed",
    create_provider: "provider_created",
    stage_provider_revision: "provider_revision_staged",
    activate_provider_revision: "provider_revision_activated",
    rollback_provider_revision: "provider_revision_rolled_back",
    inspect_model_revision: "model_revision_inspected",
    list_model_revisions: "model_revisions_listed",
    create_model: "model_created",
    stage_model_revision: "model_revision_staged",
    activate_model_revision: "model_revision_activated",
    rollback_model_revision: "model_revision_rolled_back",
    inspect_credential_generation: "credential_generation_inspected",
    list_credential_generations: "credential_generations_listed",
    create_credential_binding: "credential_binding_created",
    stage_credential_generation: "credential_generation_staged",
    activate_credential_generation: "credential_generation_activated",
    rollback_credential_generation: "credential_generation_rolled_back",
  };
  return expected[request] === response;
}

function registryBindingId(request: RuntimeRegistryAdminRequest) {
  return "binding_id" in request ? request.binding_id : undefined;
}

function applyRegistryMutation(
  current: RuntimeViewState,
  request: RuntimeRegistryAdminRequest,
  result: RegistrySuccessResult,
): RuntimeViewState {
  const registry = current.registryAdministration;
  if (result.operation === "provider_revision_inspected"
    || result.operation === "provider_created"
    || result.operation === "provider_revision_staged"
    || result.operation === "provider_revision_activated"
    || result.operation === "provider_revision_rolled_back") {
    const activated = result.operation === "provider_revision_activated"
      || result.operation === "provider_revision_rolled_back"
      || result.operation === "provider_created";
    const activeRevision = activated ? result.revision.definition.revision : registry.provider?.activeRevision ?? 0;
    return { ...current, registryAdministration: { ...registry, status: "ready", pendingOperation: undefined, notice: registrySuccessNotice(result.operation), provider: {
      id: result.revision.definition.provider_id,
      activeRevision,
      revisions: replaceRegistryRevision(registry.provider?.revisions ?? [], result.revision, activeRevision),
    } } };
  }
  if (result.operation === "model_revision_inspected"
    || result.operation === "model_created"
    || result.operation === "model_revision_staged"
    || result.operation === "model_revision_activated"
    || result.operation === "model_revision_rolled_back") {
    const activated = result.operation === "model_revision_activated"
      || result.operation === "model_revision_rolled_back"
      || result.operation === "model_created";
    const activeRevision = activated ? result.revision.definition.revision : registry.model?.activeRevision ?? 0;
    return { ...current, registryAdministration: { ...registry, status: "ready", pendingOperation: undefined, notice: registrySuccessNotice(result.operation), model: {
      providerId: result.revision.definition.provider_id,
      modelId: result.revision.definition.model_id,
      activeRevision,
      revisions: replaceRegistryRevision(registry.model?.revisions ?? [], result.revision, activeRevision),
    } } };
  }
  if (result.operation === "credential_generation_inspected"
    || result.operation === "credential_binding_created"
    || result.operation === "credential_generation_staged") {
    const bindingId = registryBindingId(request);
    if (!bindingId) return current;
    const activated = result.operation === "credential_binding_created";
    const activeGeneration = activated ? result.generation.generation : registry.credential?.activeGeneration ?? 0;
    return { ...current, registryAdministration: { ...registry, status: "ready", pendingOperation: undefined, notice: registrySuccessNotice(result.operation), credential: {
      bindingId,
      bindingIdSha256: result.generation.binding_id_sha256,
      activeGeneration,
      generations: replaceRegistryRevision(registry.credential?.generations ?? [], result.generation, activeGeneration, "generation"),
    } } };
  }
  const bindingId = registryBindingId(request);
  if (!bindingId || !registry.credential
    || (result.operation !== "credential_generation_activated"
      && result.operation !== "credential_generation_rolled_back")) return current;
  return { ...current, registryAdministration: { ...registry, status: "ready", pendingOperation: undefined, notice: registrySuccessNotice(result.operation), credential: {
    ...registry.credential,
    bindingId,
    bindingIdSha256: result.binding_id_sha256,
    activeGeneration: result.active_generation,
    generations: registry.credential.generations.map((generation) => ({ ...generation, active: generation.generation === result.active_generation })),
  } } };
}

function replaceRegistryRevision<T extends { active: boolean }>(
  values: T[],
  replacement: T,
  active: number,
  field: "revision" | "generation" = "revision",
) {
  const identity = field === "revision"
    ? (value: T) => (value as T & { definition: { revision: number } }).definition.revision
    : (value: T) => (value as T & { generation: number }).generation;
  const next = values.some((value) => identity(value) === identity(replacement))
    ? values.map((value) => identity(value) === identity(replacement) ? replacement : value)
    : [...values, replacement];
  return next.map((value) => ({ ...value, active: identity(value) === active }));
}

function registrySuccessNotice(operation: RegistrySuccessResult["operation"]) {
  return operation.includes("staged")
    ? "Registry revision staged; activation is still required"
    : `Registry ${operation.replaceAll("_", " ")}`;
}

function sameDeltaTarget(left: PendingDelta, right: PendingDelta): boolean {
  if (left.kind !== right.kind) return false;
  return left.kind !== "tool" || (right.kind === "tool" && left.callId === right.callId);
}

function applyPendingDelta(entries: TranscriptEntry[], pending: PendingDelta): TranscriptEntry[] {
  if (pending.kind === "tool") {
    return entries.map((entry) => entry.id === `tool-${pending.callId}`
      ? {
          ...entry,
          body: entry.body === RUNNING_TOOL_PLACEHOLDER
            ? pending.delta
            : entry.body + pending.delta,
        }
      : entry);
  }
  const id = `streaming-${pending.kind}`;
  const index = entries.findLastIndex((entry) => entry.id === id);
  if (index >= 0) {
    return entries.map((entry, entryIndex) => entryIndex === index
      ? { ...entry, body: entry.body + pending.delta }
      : entry);
  }
  return [...entries, { id, kind: pending.kind, body: pending.delta }];
}

function settleTurnTranscript(
  entries: TranscriptEntry[],
  sequence: number,
  finalText?: string,
  notice?: string,
): TranscriptEntry[] {
  let foundAssistant = false;
  const settled = entries.map((entry) => {
    if (entry.id === "streaming-assistant") {
      foundAssistant = true;
      return {
        ...entry,
        id: `assistant-${sequence}`,
        body: finalText ?? entry.body,
      };
    }
    if (entry.id === "streaming-thinking") {
      return { ...entry, id: `thinking-${sequence}` };
    }
    if (entry.kind === "tool" && entry.status === "running") {
      return { ...entry, status: "failed" as const };
    }
    return entry;
  });
  if (!foundAssistant && finalText) {
    settled.push({ id: `assistant-${sequence}`, kind: "assistant", body: finalText });
  }
  if (notice) {
    settled.push({ id: `notice-${sequence}`, kind: "notice", body: notice, status: "failed" });
  }
  return settled;
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

function retryCauseLabel(cause: "rate_limit" | "server" | "network" | "stream" | "other") {
  return cause === "rate_limit"
    ? "Rate limited"
    : `${cause.charAt(0).toUpperCase()}${cause.slice(1)}`;
}

function timeoutKindLabel(kind: "approval" | "question" | "plan" | "tool" | "task") {
  return kind === "plan" ? "plan review" : kind === "task" ? "background task" : kind;
}

function timeoutRecoveryLabel(recovery: "retry_request" | "narrow_scope" | "continue_without") {
  switch (recovery) {
    case "retry_request": return "retry the request";
    case "narrow_scope": return "retry with narrower scope";
    case "continue_without": return "continue without this result";
  }
}
