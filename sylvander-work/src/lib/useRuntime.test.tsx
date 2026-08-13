import { act, renderHook, waitFor } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import type { DesktopEvent, RuntimeCommand, RuntimeGatewayPort, RuntimeUserProfileData } from "./gateway";
import { useRuntime } from "./useRuntime";

class ProfileGateway implements RuntimeGatewayPort {
  commands: RuntimeCommand[] = [];
  listener?: (event: DesktopEvent) => void;

  async connect(listener: (event: DesktopEvent) => void) {
    this.listener = listener;
  }

  async submit(message: RuntimeCommand) {
    this.commands.push(message);
  }

  async disconnect() {}

  emit(event: DesktopEvent) {
    this.listener?.(event);
  }
}

const profile: RuntimeUserProfileData = {
  preferred_language: { value: "zh-CN", privacy_class: "personal" },
  response_detail: { value: "concise", privacy_class: "personal" },
  constraints: [{ value: "Do not expose secrets", privacy_class: "restricted" }],
};

describe("useRuntime user profile", () => {
  it("binds owner profile mutations to Runtime revision and reloads conflicts", async () => {
    const gateway = new ProfileGateway();
    const view = renderHook(() => useRuntime(gateway));
    await waitFor(() => expect(gateway.listener).toBeTypeOf("function"));
    act(() => gateway.emit({
      type: "connected",
      protocol: { server_name: "runtime", version: 6, capabilities: ["user_profile_v1"] },
    }));

    await act(async () => {
      expect(await view.result.current.requestUserProfile({ operation: "read" })).toBe(true);
    });
    expect(gateway.commands.at(-1)).toEqual({
      type: "user_profile",
      request: { version: 1, action: { operation: "read" } },
    });

    act(() => gateway.emit({ type: "message", message: {
      type: "user_profile",
      response: {
        result: "read",
        version: 1,
        profile: {
          revision: 4,
          profile,
          do_not_learn: false,
          created_at_unix_secs: 10,
          updated_at_unix_secs: 20,
        },
      },
    } }));
    expect(view.result.current.state.userProfile.profile?.revision).toBe(4);

    await act(async () => {
      expect(await view.result.current.requestUserProfile({
        operation: "update",
        expected_revision: 4,
        profile,
      })).toBe(true);
    });
    expect(gateway.commands.at(-1)).toMatchObject({
      type: "user_profile",
      request: { action: { operation: "update", expected_revision: 4 } },
    });

    act(() => gateway.emit({ type: "message", message: {
      type: "user_profile",
      response: {
        result: "read",
        version: 1,
        profile: {
          revision: 99,
          profile,
          do_not_learn: false,
          created_at_unix_secs: 10,
          updated_at_unix_secs: 99,
        },
      },
    } }));
    expect(view.result.current.state.userProfile.status).toBe("submitting");
    expect(view.result.current.state.userProfile.profile?.revision).toBe(4);

    act(() => gateway.emit({ type: "message", message: {
      type: "user_profile",
      response: {
        result: "error",
        version: 1,
        error: { code: "conflict", operation: "update", current_revision: 5 },
      },
    } }));
    await waitFor(() => expect(gateway.commands.at(-1)).toEqual({
      type: "user_profile",
      request: { version: 1, action: { operation: "read" } },
    }));
    expect(view.result.current.state.userProfile.profile).toBeUndefined();
    await waitFor(() => expect(view.result.current.state.userProfile.notice)
      .toMatch(/stale edit was not applied/));

    act(() => view.result.current.clearUserProfile());
    act(() => gateway.emit({ type: "message", message: {
      type: "user_profile",
      response: { result: "not_found", version: 1 },
    } }));
    expect(view.result.current.state.userProfile).toEqual({ status: "idle" });
    view.unmount();
  });

  it("does not issue profile requests without negotiated capability", async () => {
    const gateway = new ProfileGateway();
    const view = renderHook(() => useRuntime(gateway));
    await waitFor(() => expect(gateway.listener).toBeTypeOf("function"));
    act(() => gateway.emit({
      type: "connected",
      protocol: { server_name: "runtime", version: 6, capabilities: [] },
    }));
    const before = gateway.commands.length;
    await act(async () => {
      expect(await view.result.current.requestUserProfile({ operation: "read" })).toBe(false);
      expect(await view.result.current.requestIdentityBinding({ operation: "resolve" })).toBe(false);
    });
    expect(gateway.commands).toHaveLength(before);
    view.unmount();
  });

  it("keeps one-time identity proof state ephemeral and response-bound", async () => {
    const gateway = new ProfileGateway();
    const view = renderHook(() => useRuntime(gateway));
    await waitFor(() => expect(gateway.listener).toBeTypeOf("function"));
    act(() => gateway.emit({
      type: "connected",
      protocol: { server_name: "runtime", version: 6, capabilities: ["identity_binding_v1"] },
    }));

    await act(async () => {
      expect(await view.result.current.requestIdentityBinding({ operation: "begin" })).toBe(true);
    });
    expect(gateway.commands.at(-1)).toEqual({
      type: "identity_binding",
      request: { version: 1, action: { operation: "begin" } },
    });
    act(() => gateway.emit({ type: "message", message: {
      type: "identity_binding",
      response: {
        result: "challenge_issued",
        version: 1,
        challenge_id: "challenge-1",
        secret: "one-time-secret-123",
        expires_at_unix_secs: 100,
      },
    } }));
    expect(view.result.current.state.identityBinding.challenge).toEqual({
      id: "challenge-1",
      secret: "one-time-secret-123",
      expiresAtUnixSecs: 100,
    });

    await act(async () => {
      expect(await view.result.current.requestIdentityBinding({ operation: "resolve" })).toBe(true);
    });
    expect(view.result.current.state.identityBinding.challenge).toBeUndefined();
    act(() => gateway.emit({ type: "message", message: {
      type: "identity_binding",
      response: {
        result: "challenge_issued",
        version: 1,
        challenge_id: "stale-challenge",
        secret: "stale-secret-value",
        expires_at_unix_secs: 200,
      },
    } }));
    expect(view.result.current.state.identityBinding.status).toBe("loading");
    act(() => gateway.emit({ type: "message", message: {
      type: "identity_binding",
      response: {
        result: "resolved",
        version: 1,
        binding: { user_id: "alice", revision: 2, linked_at_unix_secs: 90 },
      },
    } }));
    expect(view.result.current.state.identityBinding.binding).toEqual({
      user_id: "alice",
      revision: 2,
      linked_at_unix_secs: 90,
    });

    await act(async () => {
      expect(await view.result.current.requestIdentityBinding({
        operation: "unlink",
        expected_revision: 2,
      })).toBe(true);
    });
    act(() => gateway.emit({ type: "message", message: {
      type: "identity_binding",
      response: {
        result: "error",
        version: 1,
        error: {
          code: "conflict",
          operation: "unlink",
          message: "identity binding revision changed",
        },
      },
    } }));
    expect(view.result.current.state.identityBinding.status).toBe("error");
    expect(view.result.current.state.identityBinding.binding?.revision).toBe(2);

    act(() => view.result.current.clearIdentityBinding());
    act(() => gateway.emit({ type: "message", message: {
      type: "identity_binding",
      response: { result: "unlinked", version: 1 },
    } }));
    expect(view.result.current.state.identityBinding).toEqual({ status: "idle" });
    view.unmount();
  });

  it("projects only redacted Agent revisions and waits for activation facts", async () => {
    const gateway = new ProfileGateway();
    const view = renderHook(() => useRuntime(gateway));
    await waitFor(() => expect(gateway.listener).toBeTypeOf("function"));
    act(() => gateway.emit({
      type: "connected",
      protocol: { server_name: "runtime", version: 6, capabilities: ["agent_administration"] },
    }));
    await act(async () => {
      expect(await view.result.current.requestAgentAdministration({
        operation: "list_revisions",
        agent_id: "agent-1",
        limit: 50,
      })).toBe(true);
    });
    expect(gateway.commands.at(-1)).toEqual({
      type: "agent_admin",
      request: { operation: "list_revisions", agent_id: "agent-1", limit: 50 },
    });
    const revision = {
      definition: {
        agent_id: "agent-1",
        revision: 4,
        name: "Coding Agent",
        description: "Works on code",
        provider_id: "openai",
        default_model_id: "gpt-test",
        allowed_models: [{ provider_id: "openai", model_id: "gpt-test" }],
        system_prompt_sha256: "sha256:prompt",
        tools: [{ type: "builtin" as const, name: "Read" }],
        memory_store_types: ["sqlite"],
        ui_commands: [],
        hooks: [],
        tool_presentations: [],
        behavior: { max_iterations: 50, max_retries: 3 },
        agent_workspace_configured: true,
        workspace_mount_count: 1,
        prompt_profiles: [],
        allow_session_prompt: false,
        access: { allow_authenticated: true, allowed_principal_count: 0, allowed_roles: [] },
      },
      digest_sha256: "sha256:definition",
      created_at_unix_secs: 100,
      active: true,
    };
    act(() => gateway.emit({ type: "message", message: {
      type: "agent_admin",
      response: {
        status: "success",
        result: {
          operation: "revisions_listed",
          agent_id: "agent-1",
          active_revision: 4,
          revisions: [revision, {
            ...revision,
            definition: { ...revision.definition, revision: 3 },
            digest_sha256: "sha256:older",
            active: false,
          }],
        },
      },
    } }));
    expect(view.result.current.state.agentAdministration.activeRevision).toBe(4);

    await act(async () => {
      expect(await view.result.current.requestAgentAdministration({
        operation: "update_definition",
        expected_active_revision: 4,
        definition: {
          agent_id: "agent-1",
          revision: 5,
          name: "Coding Agent",
          description: "Works on code",
          provider_id: "openai",
          default_model_id: "gpt-test",
          allowed_models: [{ provider_id: "openai", model_id: "gpt-test" }],
          system_prompt: "new write-only prompt",
          tools: [{ type: "builtin", name: "Read" }],
          memory_stores: [],
          ui_commands: [],
          hooks: [],
          tool_presentations: [],
          behavior: { max_iterations: 50, max_retries: 3 },
          workspace_mounts: [],
          prompt_profiles: [],
          allow_session_prompt: false,
          access: { allow_authenticated: true, allowed_principals: [], allowed_roles: [] },
        },
      })).toBe(true);
    });
    expect(view.result.current.state.agentAdministration.activeRevision).toBe(4);
    act(() => gateway.emit({ type: "message", message: {
      type: "agent_admin",
      response: {
        status: "success",
        result: {
          operation: "definition_updated",
          revision: {
            ...revision,
            definition: {
              ...revision.definition,
              revision: 5,
              system_prompt_sha256: "sha256:new-prompt",
            },
            digest_sha256: "sha256:new-definition",
            active: false,
          },
        },
      },
    } }));
    expect(view.result.current.state.agentAdministration.revisions.some(
      (candidate) => candidate.definition.revision === 5 && !candidate.active,
    )).toBe(true);
    expect(view.result.current.state.agentAdministration.notice).toMatch(/activation is still required/);

    await act(async () => {
      expect(await view.result.current.requestAgentAdministration({
        operation: "rollback_revision",
        agent_id: "agent-1",
        target_revision: 3,
        expected_active_revision: 4,
      })).toBe(true);
    });
    expect(view.result.current.state.agentAdministration.activeRevision).toBe(4);
    act(() => gateway.emit({ type: "message", message: {
      type: "agent_admin",
      response: {
        status: "success",
        result: { operation: "revision_rolled_back", agent_id: "agent-1", active_revision: 3 },
      },
    } }));
    expect(view.result.current.state.agentAdministration.activeRevision).toBe(3);
    expect(view.result.current.state.agentAdministration.revisions.find(
      (candidate) => candidate.definition.revision === 3,
    )?.active).toBe(true);

    await act(async () => {
      expect(await view.result.current.requestAgentAdministration({
        operation: "activate_revision",
        agent_id: "agent-1",
        revision: 5,
        expected_active_revision: 3,
      })).toBe(true);
    });
    act(() => gateway.emit({ type: "message", message: {
      type: "agent_admin",
      response: {
        status: "error",
        error: {
          code: "revision_conflict",
          message: "active revision changed",
          agent_id: "agent-1",
          expected_active_revision: 3,
          actual_active_revision: 4,
        },
      },
    } }));
    await waitFor(() => expect(gateway.commands.at(-1)).toEqual({
      type: "agent_admin",
      request: { operation: "list_revisions", agent_id: "agent-1", limit: 50 },
    }));
    expect(view.result.current.state.agentAdministration.notice).toBe("active revision changed");
    view.unmount();
  });

  it("keeps Provider, Model, and Credential registry lifecycles revision-bound", async () => {
    const gateway = new ProfileGateway();
    const view = renderHook(() => useRuntime(gateway));
    await waitFor(() => expect(gateway.listener).toBeTypeOf("function"));
    act(() => gateway.emit({
      type: "connected",
      protocol: { server_name: "runtime", version: 6, capabilities: ["registry_administration"] },
    }));

    await act(async () => {
      expect(await view.result.current.requestRegistryAdministration({
        operation: "list_provider_revisions",
        provider_id: "openai",
        limit: 50,
      })).toBe(true);
    });
    act(() => gateway.emit({ type: "message", message: {
      type: "registry_admin",
      response: { status: "success", result: {
        operation: "provider_revisions_listed",
        provider_id: "openai",
        active_revision: 1,
        revisions: [providerRevision(1, true)],
      } },
    } }));
    expect(view.result.current.state.registryAdministration.provider?.activeRevision).toBe(1);

    await act(async () => {
      expect(await view.result.current.requestRegistryAdministration({
        operation: "stage_provider_revision",
        provider_id: "openai",
        revision: 2,
        expected_active_revision: 1,
        definition: {
          kind: "openai",
          features: ["responses"],
          base_url: "https://api.openai.com",
          credential_binding_id: "openai-primary",
        },
      })).toBe(true);
    });
    act(() => gateway.emit({ type: "message", message: {
      type: "registry_admin",
      response: { status: "success", result: {
        operation: "provider_revision_staged",
        revision: providerRevision(2, false),
      } },
    } }));
    expect(view.result.current.state.registryAdministration.provider?.activeRevision).toBe(1);
    expect(view.result.current.state.registryAdministration.provider?.revisions).toHaveLength(2);

    await act(async () => {
      expect(await view.result.current.requestRegistryAdministration({
        operation: "list_model_revisions",
        provider_id: "openai",
        model_id: "gpt-test",
        limit: 50,
      })).toBe(true);
    });
    act(() => gateway.emit({ type: "message", message: {
      type: "registry_admin",
      response: { status: "success", result: {
        operation: "model_revisions_listed",
        provider_id: "openai",
        model_id: "gpt-test",
        active_revision: 3,
        revisions: [modelRevision(3, true)],
      } },
    } }));
    expect(view.result.current.state.registryAdministration.model?.activeRevision).toBe(3);

    await act(async () => {
      expect(await view.result.current.requestRegistryAdministration({
        operation: "list_credential_generations",
        binding_id: "openai-primary",
        limit: 50,
      })).toBe(true);
    });
    act(() => gateway.emit({ type: "message", message: {
      type: "registry_admin",
      response: { status: "success", result: {
        operation: "credential_generations_listed",
        binding_id_sha256: "sha256:binding",
        active_generation: 2,
        generations: [credentialGeneration(2, true), credentialGeneration(1, false)],
      } },
    } }));
    expect(view.result.current.state.registryAdministration.credential).toMatchObject({
      bindingId: "openai-primary",
      bindingIdSha256: "sha256:binding",
      activeGeneration: 2,
    });

    await act(async () => {
      expect(await view.result.current.requestRegistryAdministration({
        operation: "rollback_credential_generation",
        binding_id: "openai-primary",
        target_generation: 1,
        expected_active_generation: 2,
      })).toBe(true);
    });
    expect(view.result.current.state.registryAdministration.credential?.activeGeneration).toBe(2);
    act(() => gateway.emit({ type: "message", message: {
      type: "registry_admin",
      response: { status: "success", result: {
        operation: "credential_generation_rolled_back",
        binding_id_sha256: "sha256:binding",
        active_generation: 1,
      } },
    } }));
    expect(view.result.current.state.registryAdministration.credential?.activeGeneration).toBe(1);

    await act(async () => {
      expect(await view.result.current.requestRegistryAdministration({
        operation: "activate_provider_revision",
        provider_id: "openai",
        revision: 2,
        expected_active_revision: 1,
      })).toBe(true);
    });
    act(() => gateway.emit({ type: "message", message: {
      type: "registry_admin",
      response: { status: "error", error: {
        code: "active_revision_conflict",
        message: "provider active revision changed",
        provider_id: "openai",
        details: { kind: "active_revision_conflict", expected_active_revision: 1, actual_active_revision: 3 },
      } },
    } }));
    await waitFor(() => expect(gateway.commands.at(-1)).toEqual({
      type: "registry_admin",
      request: { operation: "list_provider_revisions", provider_id: "openai", limit: 50 },
    }));
    expect(view.result.current.state.registryAdministration.notice).toBe("provider active revision changed");
    view.unmount();
  });
});

function providerRevision(revision: number, active: boolean) {
  return {
    definition: {
      provider_id: "openai", revision, kind: "openai", features: ["responses"],
      base_url_sha256: `sha256:base-${revision}`,
      credential_binding_id_sha256: "sha256:binding",
    },
    digest_sha256: `sha256:provider-${revision}`,
    created_at_unix_secs: revision,
    active,
  };
}

function modelRevision(revision: number, active: boolean) {
  return {
    definition: {
      provider_id: "openai", model_id: "gpt-test", revision,
      context_window: 128_000, max_output_tokens: 8_192,
      capabilities: ["tool_use"], lifecycle: { status: "active" as const },
      pricing_sha256: "sha256:pricing",
    },
    digest_sha256: `sha256:model-${revision}`,
    created_at_unix_secs: revision,
    active,
  };
}

function credentialGeneration(generation: number, active: boolean) {
  return {
    binding_id_sha256: "sha256:binding", generation,
    reference_kind: "environment" as const,
    reference_configured: true,
    reference_digest_sha256: `sha256:reference-${generation}`,
    created_at_unix_secs: generation,
    active,
  };
}
