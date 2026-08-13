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
});
