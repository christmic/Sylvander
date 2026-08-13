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
    });
    expect(gateway.commands).toHaveLength(before);
    view.unmount();
  });
});
