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
  | { type: "chat"; text: string; attachments: RuntimeMessageAttachment[]; session_id?: string }
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
  | { type: "submit_feedback"; feedback: RuntimeFeedback }
  | { type: "memory_confirmation"; request: RuntimeMemoryConfirmationRequest }
  | { type: "user_profile"; request: RuntimeUserProfileRequest }
  | { type: "identity_binding"; request: RuntimeIdentityBindingRequest }
  | { type: "agent_admin"; request: RuntimeAgentAdminRequest }
  | { type: "registry_admin"; request: RuntimeRegistryAdminRequest }
  | { type: "ping" };

export type PlanDecision =
  | { decision: "approved" }
  | { decision: "revised"; steps: string[] }
  | { decision: "rejected"; reason: string };

export type ApprovalScope = "once" | "session" | "persistent";
export type ReasoningEffort = "off" | "low" | "medium" | "high";

export interface RuntimeMessageAttachment {
  id: string;
  kind: "paste" | "file" | "image" | "selection" | "diff" | "terminal_output";
  name: string;
  mime_type: string;
  content: { encoding: "text"; text: string } | { encoding: "base64"; data: string };
  byte_count: number;
}

export interface RuntimeFeedback {
  target: string;
  rating: "positive" | "negative";
  note?: string;
  tags: string[];
  artifacts: never[];
  validations: never[];
  privacy_class: "private";
}

export type RuntimeMemoryConfirmationRequest =
  | { operation: "list"; version: 1; session_id: string }
  | {
      operation: "decide";
      version: 1;
      session_id: string;
      candidate_id: string;
      expected_revision: number;
      decision: "confirm" | "reject";
    };

export interface RuntimePendingMemoryConfirmation {
  candidate_id: string;
  expected_revision: number;
  scope: "relationship" | "user_profile" | "agent_canonical" | "workspace_knowledge";
  summary: string;
}

type RuntimeMemoryConfirmationResponse =
  | {
      result: "pending";
      version: 1;
      session_id: string;
      confirmations: RuntimePendingMemoryConfirmation[];
    }
  | {
      result: "recorded";
      version: 1;
      session_id: string;
      candidate_id: string;
      decision: "confirm" | "reject";
    }
  | {
      result: "error";
      version: 1;
      operation: string;
      code: "unsupported_version" | "invalid_request" | "unauthenticated" | "forbidden" | "conflict" | "service_unavailable";
      message: string;
    };

export type RuntimePrivacyClass = "personal" | "sensitive" | "restricted";

export interface RuntimeClassifiedPreference<T> {
  value: T;
  privacy_class: RuntimePrivacyClass;
}

export interface RuntimeUserProfileData {
  preferred_language?: RuntimeClassifiedPreference<string>;
  locale?: RuntimeClassifiedPreference<string>;
  response_detail?: RuntimeClassifiedPreference<"concise" | "balanced" | "detailed">;
  communication_tone?: RuntimeClassifiedPreference<"direct" | "warm" | "formal">;
  accessibility?: RuntimeClassifiedPreference<{
    screen_reader_optimized: boolean;
    reduce_motion: boolean;
    high_contrast: boolean;
  }>;
  constraints: Array<RuntimeClassifiedPreference<string>>;
}

export interface RuntimeUserProfileView {
  revision: number;
  profile: RuntimeUserProfileData;
  do_not_learn: boolean;
  created_at_unix_secs: number;
  updated_at_unix_secs: number;
}

export type RuntimeUserProfileAction =
  | { operation: "create"; profile: RuntimeUserProfileData }
  | { operation: "read" }
  | { operation: "update"; expected_revision: number; profile: RuntimeUserProfileData }
  | { operation: "export"; format: "json" }
  | { operation: "correct"; expected_revision: number; profile: RuntimeUserProfileData }
  | { operation: "delete"; expected_revision: number }
  | { operation: "set_do_not_learn"; expected_revision: number; enabled: boolean };

export interface RuntimeUserProfileRequest {
  version: 1;
  action: RuntimeUserProfileAction;
}

export interface RuntimeUserProfileExport {
  schema_version: number;
  format: "json";
  profile: RuntimeUserProfileView;
  exported_at_unix_secs: number;
}

export type RuntimeUserProfileOperation = RuntimeUserProfileAction["operation"];

export type RuntimeUserProfileResponse =
  | { result: "created" | "read" | "updated" | "corrected" | "do_not_learn_updated"; version: 1; profile: RuntimeUserProfileView }
  | { result: "exported"; version: 1; export: RuntimeUserProfileExport }
  | { result: "deleted"; version: 1; deleted_revision: number; do_not_learn_preserved: boolean }
  | { result: "not_found"; version: 1 }
  | {
      result: "error";
      version: 1;
      error: {
        code: "unsupported_version" | "invalid_request" | "unauthenticated" | "forbidden" | "not_found" | "already_exists" | "conflict" | "rate_limited" | "service_unavailable" | "internal";
        operation: RuntimeUserProfileOperation;
        current_revision?: number;
        retry_after_ms?: number;
      };
    };

export type RuntimeIdentityBindingAction =
  | { operation: "begin" }
  | { operation: "confirm"; challenge_id: string; proof: string }
  | { operation: "resolve" }
  | { operation: "unlink"; expected_revision: number };

export interface RuntimeIdentityBindingRequest {
  version: 1;
  action: RuntimeIdentityBindingAction;
}

export interface RuntimeIdentityBindingView {
  user_id: string;
  revision: number;
  linked_at_unix_secs: number;
}

export type RuntimeIdentityBindingResponse =
  | {
      result: "challenge_issued";
      version: 1;
      challenge_id: string;
      secret: string;
      expires_at_unix_secs: number;
    }
  | { result: "resolved"; version: 1; binding: RuntimeIdentityBindingView }
  | { result: "not_linked"; version: 1 }
  | { result: "unlinked"; version: 1 }
  | {
      result: "error";
      version: 1;
      error: {
        code: "unsupported_version" | "invalid_request" | "unauthenticated" | "forbidden" | "already_linked" | "not_linked" | "challenge_unavailable" | "challenge_expired" | "challenge_rejected" | "conflict" | "rate_limited" | "service_unavailable" | "internal";
        operation: RuntimeIdentityBindingAction["operation"];
        message: string;
        retry_after_ms?: number;
      };
    };

export type RuntimeAgentAdminRequest =
  | { operation: "inspect_revision"; agent_id: string; revision: number }
  | { operation: "list_revisions"; agent_id: string; before_revision?: number; limit: number }
  | { operation: "update_definition"; expected_active_revision: number; definition: RuntimeAgentDefinitionDraft }
  | { operation: "activate_revision"; agent_id: string; revision: number; expected_active_revision: number }
  | { operation: "rollback_revision"; agent_id: string; target_revision: number; expected_active_revision: number };

export interface RuntimeModelSelection {
  provider_id: string;
  model_id: string;
}

export type RuntimeAgentSecretReference =
  | { source: "environment"; name: string }
  | { source: "file"; path: string };

export type RuntimeAgentToolDraft =
  | { type: "builtin"; name: string }
  | {
      type: "mcp_server";
      name: string;
      execution_environment: string;
      workspace_access: "read" | "write";
      command: string;
      args: string[];
      environment: Record<string, RuntimeAgentSecretReference>;
    };

export interface RuntimeWorkspaceBinding {
  execution_target: string;
  path: string;
  read_only: boolean;
  instruction_focus?: string;
}

export interface RuntimeWorkspaceMount {
  reference: string;
  role: "agent_home" | "task" | "dependency" | "artifact";
  binding: RuntimeWorkspaceBinding;
  capabilities: { read: boolean; write: boolean; command: boolean; git: boolean };
}

export interface RuntimeAgentDefinitionDraft {
  agent_id: string;
  revision: number;
  name: string;
  description: string;
  provider_id: string;
  default_model_id: string;
  allowed_models: RuntimeModelSelection[];
  temperature?: number;
  max_tokens?: number;
  system_prompt: string;
  tools: RuntimeAgentToolDraft[];
  memory_stores: Array<{ store_type: string; path: string }>;
  ui_commands: Array<{
    id: string;
    name: string;
    usage: string;
    description: string;
    hint: string;
    prompt: string;
  }>;
  hooks: Array<{
    name: string;
    phase: "before_tool" | "after_tool" | "before_turn" | "after_turn";
    command: string;
    timeout_secs: number;
    blocking: boolean;
  }>;
  tool_presentations: Array<{
    tool_name: string;
    label: string;
    kind: "generic" | "command" | "file" | "search" | "resource";
    target_field?: string;
  }>;
  behavior: { max_iterations: number; max_retries: number };
  agent_workspace?: RuntimeWorkspaceBinding;
  workspace_mounts: RuntimeWorkspaceMount[];
  prompt_profiles: Array<{
    id: string;
    qualified_models: RuntimeModelSelection[];
    system_prompt: string;
  }>;
  default_prompt_profile?: string;
  allow_session_prompt: boolean;
  access: { allow_authenticated: boolean; allowed_principals: string[]; allowed_roles: string[] };
}

export interface RuntimeAgentRevisionView {
  definition: {
    agent_id: string;
    revision: number;
    name: string;
    description: string;
    provider_id: string;
    default_model_id: string;
    allowed_models: Array<{ provider_id: string; model_id: string }>;
    system_prompt_sha256: string;
    tools: Array<{ type: "builtin"; name: string } | { type: "mcp_server"; name: string }>;
    memory_store_types: string[];
    ui_commands: Array<{ id: string; name: string; usage: string; description: string; hint: string }>;
    hooks: Array<{ name: string; phase: "before_tool" | "after_tool" | "before_turn" | "after_turn"; timeout_secs: number; blocking: boolean }>;
    tool_presentations: Array<{ tool_name: string; label: string; kind: string; target_field?: string }>;
    behavior: { max_iterations: number; max_retries: number };
    agent_workspace_configured: boolean;
    workspace_mount_count: number;
    prompt_profiles: Array<{ id: string; qualified_models: Array<{ provider_id: string; model_id: string }>; system_prompt_sha256: string }>;
    default_prompt_profile?: string;
    allow_session_prompt: boolean;
    access: { allow_authenticated: boolean; allowed_principal_count: number; allowed_roles: string[] };
  };
  digest_sha256: string;
  created_at_unix_secs: number;
  active: boolean;
}

export type RuntimeAgentAdminResponse =
  | { status: "success"; result: { operation: "revision_inspected"; revision: RuntimeAgentRevisionView } }
  | { status: "success"; result: { operation: "revisions_listed"; agent_id: string; active_revision: number; revisions: RuntimeAgentRevisionView[]; next_before_revision?: number } }
  | { status: "success"; result: { operation: "definition_updated"; revision: RuntimeAgentRevisionView } }
  | { status: "success"; result: { operation: "revision_activated" | "revision_rolled_back"; agent_id: string; active_revision: number } }
  | { status: "error"; error: { code: string; message: string; agent_id?: string; revision?: number; expected_active_revision?: number; actual_active_revision?: number } };

export interface RuntimeProviderDefinitionDraft {
  kind: string;
  features: string[];
  base_url: string;
  credential_binding_id: string;
}

export interface RuntimeModelDefinitionDraft {
  context_window: number;
  max_output_tokens: number;
  capabilities: string[];
  lifecycle: { status: "active" } | { status: "deprecated"; replacement?: string };
  pricing?: {
    input_usd_micros_per_million: number;
    output_usd_micros_per_million: number;
    cache_write_usd_micros_per_million?: number;
    cache_read_usd_micros_per_million?: number;
  };
}

export type RuntimeCredentialSecretReference =
  | { source: "environment"; name: string }
  | { source: "file"; path: string };

export type RuntimeRegistryAdminRequest =
  | { operation: "inspect_provider_revision"; provider_id: string; revision: number }
  | { operation: "list_provider_revisions"; provider_id: string; before_revision?: number; limit: number }
  | { operation: "create_provider"; provider_id: string; definition: RuntimeProviderDefinitionDraft }
  | { operation: "stage_provider_revision"; provider_id: string; revision: number; expected_active_revision: number; definition: RuntimeProviderDefinitionDraft }
  | { operation: "activate_provider_revision"; provider_id: string; revision: number; expected_active_revision: number }
  | { operation: "rollback_provider_revision"; provider_id: string; target_revision: number; expected_active_revision: number }
  | { operation: "inspect_model_revision"; provider_id: string; model_id: string; revision: number }
  | { operation: "list_model_revisions"; provider_id: string; model_id: string; before_revision?: number; limit: number }
  | { operation: "create_model"; provider_id: string; model_id: string; definition: RuntimeModelDefinitionDraft }
  | { operation: "stage_model_revision"; provider_id: string; model_id: string; revision: number; expected_active_revision: number; definition: RuntimeModelDefinitionDraft }
  | { operation: "activate_model_revision"; provider_id: string; model_id: string; revision: number; expected_active_revision: number }
  | { operation: "rollback_model_revision"; provider_id: string; model_id: string; target_revision: number; expected_active_revision: number }
  | { operation: "inspect_credential_generation"; binding_id: string; generation: number }
  | { operation: "list_credential_generations"; binding_id: string; before_generation?: number; limit: number }
  | { operation: "create_credential_binding"; binding_id: string; reference: RuntimeCredentialSecretReference }
  | { operation: "stage_credential_generation"; binding_id: string; generation: number; expected_active_generation: number; reference: RuntimeCredentialSecretReference }
  | { operation: "activate_credential_generation"; binding_id: string; generation: number; expected_active_generation: number }
  | { operation: "rollback_credential_generation"; binding_id: string; target_generation: number; expected_active_generation: number };

export interface RuntimeProviderRevisionView {
  definition: {
    provider_id: string;
    revision: number;
    kind: string;
    features: string[];
    base_url_sha256: string;
    credential_binding_id_sha256: string;
  };
  digest_sha256: string;
  created_at_unix_secs: number;
  active: boolean;
}

export interface RuntimeModelRevisionView {
  definition: {
    provider_id: string;
    model_id: string;
    revision: number;
    context_window: number;
    max_output_tokens: number;
    capabilities: string[];
    lifecycle: { status: "active" } | { status: "deprecated"; replacement?: string };
    pricing_sha256?: string;
  };
  digest_sha256: string;
  created_at_unix_secs: number;
  active: boolean;
}

export interface RuntimeCredentialGenerationView {
  binding_id_sha256: string;
  generation: number;
  reference_kind: "environment" | "file";
  reference_configured: boolean;
  reference_digest_sha256: string;
  created_at_unix_secs: number;
  active: boolean;
}

type RuntimeRegistrySuccessResult =
  | { operation: "provider_revision_inspected"; revision: RuntimeProviderRevisionView }
  | { operation: "provider_revisions_listed"; provider_id: string; active_revision: number; revisions: RuntimeProviderRevisionView[]; next_before_revision?: number }
  | { operation: "provider_created" | "provider_revision_staged" | "provider_revision_activated" | "provider_revision_rolled_back"; revision: RuntimeProviderRevisionView }
  | { operation: "model_revision_inspected"; revision: RuntimeModelRevisionView }
  | { operation: "model_revisions_listed"; provider_id: string; model_id: string; active_revision: number; revisions: RuntimeModelRevisionView[]; next_before_revision?: number }
  | { operation: "model_created" | "model_revision_staged" | "model_revision_activated" | "model_revision_rolled_back"; revision: RuntimeModelRevisionView }
  | { operation: "credential_generation_inspected"; generation: RuntimeCredentialGenerationView }
  | { operation: "credential_generations_listed"; binding_id_sha256: string; active_generation: number; generations: RuntimeCredentialGenerationView[]; next_before_generation?: number }
  | { operation: "credential_binding_created" | "credential_generation_staged"; generation: RuntimeCredentialGenerationView }
  | { operation: "credential_generation_activated" | "credential_generation_rolled_back"; binding_id_sha256: string; active_generation: number };

export type RuntimeRegistryAdminResponse =
  | { status: "success"; result: RuntimeRegistrySuccessResult }
  | {
      status: "error";
      error: {
        code: string;
        message: string;
        provider_id?: string;
        model_id?: string;
        binding_id_sha256?: string;
        revision?: number;
        generation?: number;
        details?: { kind: string; [key: string]: string | number };
      };
    };

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
  | { type: "turn_started"; session_id: string; turn_id: string }
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
  | { type: "done"; session_id: string; text: string; feedback_target?: string }
  | { type: "error"; session_id: string; message: string; feedback_target?: string }
  | { type: "turn_interrupted"; session_id: string; reason: string; feedback_target?: string }
  | { type: "feedback_recorded"; feedback_id: string }
  | { type: "memory_confirmation"; response: RuntimeMemoryConfirmationResponse }
  | { type: "user_profile"; response: RuntimeUserProfileResponse }
  | { type: "identity_binding"; response: RuntimeIdentityBindingResponse }
  | { type: "agent_admin"; response: RuntimeAgentAdminResponse }
  | { type: "registry_admin"; response: RuntimeRegistryAdminResponse }
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
