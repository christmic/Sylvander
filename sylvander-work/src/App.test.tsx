import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import App from "./App";
import type { DesktopEvent, RuntimeCommand, RuntimeGatewayPort } from "./lib/gateway";
import type { DesktopHostPort, DesktopHostPreferences } from "./lib/host";

afterEach(cleanup);

class TestGateway implements RuntimeGatewayPort {
  commands: RuntimeCommand[] = [];
  connects = 0;
  rejectChat = false;
  listener?: (event: DesktopEvent) => void;

  async connect(listener: (event: DesktopEvent) => void) {
    this.connects += 1;
    this.listener = listener;
  }

  async submit(message: RuntimeCommand) {
    if (this.rejectChat && message.type === "chat") throw "Runtime command queue is unavailable";
    this.commands.push(message);
  }

  async disconnect() {}

  emit(event: DesktopEvent) {
    this.listener?.(event);
  }
}

class TestHost implements DesktopHostPort {
  preferences: DesktopHostPreferences = { turn_notifications: false };
  rejectWrites = false;
  writes: boolean[] = [];

  async getPreferences() {
    return this.preferences;
  }

  async setTurnNotifications(enabled: boolean) {
    this.writes.push(enabled);
    if (this.rejectWrites) throw new Error("save failed");
    this.preferences = { turn_notifications: enabled };
    return this.preferences;
  }
}

describe("Sylvander Work", () => {
  it("loads Runtime-owned Sessions and batches streaming deltas", async () => {
    const gateway = new TestGateway();
    render(<App gateway={gateway} />);

    await waitFor(() => expect(gateway.listener).toBeTypeOf("function"));
    act(() => gateway.emit({
      type: "connected",
      protocol: { server_name: "test-runtime", version: 5, capabilities: [] },
    }));
    act(() => gateway.emit({ type: "message", message: {
      type: "agents_discovered",
      agents: [{ id: "agent-1", revision: 1, name: "Agent", provider_id: "openai", default_model_id: "gpt-test" }],
    } }));

    await waitFor(() => expect(gateway.commands.map((command) => command.type)).toEqual([
      "discover_agents",
      "list_sessions",
      "get_runtime_info",
    ]));
    act(() => gateway.emit({ type: "message", message: {
      type: "runtime_info",
      snapshot: {
        agent_id: "agent-1",
        model: { provider_id: "openai", model_id: "gpt-test" },
        reasoning_effort: "medium",
        models: [],
        permissions: {
          file_access: "workspace_write",
          network_access: "denied",
          approval_policy: "ask",
        },
        capabilities: 0,
        approval_enabled: true,
        max_request_bytes: 1_024,
        platform: {},
      },
    } }));
    expect(await screen.findByRole("button", { name: /openai\/gpt-test/ })).toBeTruthy();
    expect(screen.getByRole("button", { name: /Medium reasoning/ })).toBeTruthy();
    expect(screen.getByText(/workspace write · network denied · approval ask/)).toBeTruthy();

    act(() => gateway.emit({
      type: "message",
      message: {
        type: "sessions_list",
        include_archived: false,
        sessions: [{ id: "session-1", label: "Long-term desktop", workspace: "/workspace", last_seen_secs: 4, archived: false }],
      },
    }));

    await waitFor(() => expect(gateway.commands.at(-1)).toEqual({ type: "load_session", session_id: "session-1" }));
    expect(within(screen.getByLabelText("Sessions")).getByText("Long-term desktop")).toBeTruthy();
    expect(within(screen.getByLabelText("Sessions")).getByText("test-runtime")).toBeTruthy();

    act(() => {
      gateway.emit({ type: "message", message: { type: "text_delta", session_id: "session-1", delta: "Hello " } });
      gateway.emit({ type: "message", message: { type: "text_delta", session_id: "session-1", delta: "world" } });
    });

    expect(await screen.findByText("Hello world")).toBeTruthy();
    act(() => {
      gateway.emit({ type: "message", message: { type: "thinking_delta", session_id: "session-1", delta: "Check " } });
      gateway.emit({ type: "message", message: { type: "thinking_delta", session_id: "session-1", delta: "facts" } });
      gateway.emit({ type: "message", message: {
        type: "tool_call", session_id: "session-1", call_id: "call-1", tool_name: "Read", input: {},
      } });
      gateway.emit({ type: "message", message: {
        type: "tool_output_delta", session_id: "session-1", call_id: "call-1", tool_name: "Read", delta: "line ",
      } });
      gateway.emit({ type: "message", message: {
        type: "tool_output_delta", session_id: "session-1", call_id: "call-1", tool_name: "Read", delta: "one",
      } });
    });
    expect(await screen.findByText("Check facts")).toBeTruthy();
    expect(await screen.findByText("line one")).toBeTruthy();
    act(() => gateway.emit({ type: "message", message: {
      type: "tool_result", session_id: "session-1", call_id: "call-1", tool_name: "Read", output: "verified output", is_error: false,
    } }));
    expect(await screen.findByText("verified output")).toBeTruthy();
    expect(screen.queryByText("line one")).toBeNull();
    act(() => gateway.emit({ type: "message", message: {
      type: "done", session_id: "session-1", text: "Hello world",
    } }));
    act(() => gateway.emit({ type: "message", message: {
      type: "text_delta", session_id: "session-1", delta: "Second turn",
    } }));
    expect(await screen.findByText("Second turn")).toBeTruthy();
    expect(screen.getByText("Hello world")).toBeTruthy();
    act(() => gateway.emit({ type: "message", message: {
      type: "error", session_id: "session-1", message: "Provider unavailable",
    } }));
    expect(await screen.findByText("Provider unavailable")).toBeTruthy();
    expect(screen.getByText("Second turn")).toBeTruthy();
  });

  it("keeps the production shell useful when Runtime is offline", async () => {
    const gateway = new TestGateway();
    render(<App gateway={gateway} />);

    await waitFor(() => expect(gateway.listener).toBeTypeOf("function"));
    act(() => gateway.emit({ type: "disconnected", reason: "Runtime endpoint is unavailable" }));

    expect(await screen.findByText("Runtime is unavailable.")).toBeTruthy();
    expect(screen.getByText("Runtime endpoint is unavailable")).toBeTruthy();
    expect(screen.getByRole("heading", { name: "Sylvander Work" })).toBeTruthy();
  });

  it("creates a Session through a Runtime-discovered Agent identity", async () => {
    const gateway = new TestGateway();
    render(<App gateway={gateway} />);
    await waitFor(() => expect(gateway.listener).toBeTypeOf("function"));
    act(() => gateway.emit({
      type: "connected",
      protocol: { server_name: "test-runtime", version: 5, capabilities: [] },
    }));
    act(() => gateway.emit({ type: "message", message: {
      type: "agents_discovered",
      agents: [{
        id: "agent-1",
        revision: 1,
        name: "Coding Agent",
        provider_id: "openai",
        default_model_id: "gpt-test",
      }],
    } }));

    const createButton = await screen.findByRole("button", { name: "Create Session" });
    expect(createButton.hasAttribute("disabled")).toBe(false);
    act(() => createButton.click());
    fireEvent.change(screen.getByRole("textbox", { name: "Session name" }), {
      target: { value: "Release work" },
    });
    act(() => screen.getByRole("button", { name: "Create" }).click());
    await waitFor(() => expect(gateway.commands.at(-1)).toEqual({
      type: "create_session",
      request: {
        agent_id: "agent-1",
        label: "Release work",
        overrides: {},
      },
    }));
    act(() => gateway.emit({ type: "message", message: {
      type: "session_created", session_id: "session-new",
    } }));
    await waitFor(() => expect(gateway.commands.slice(-2)).toEqual([
      { type: "list_sessions", include_archived: false },
      { type: "load_session", session_id: "session-new" },
    ]));
  });

  it("waits for Runtime facts before renaming, archiving, or deleting Sessions", async () => {
    const gateway = new TestGateway();
    render(<App gateway={gateway} />);
    await waitFor(() => expect(gateway.listener).toBeTypeOf("function"));
    act(() => gateway.emit({
      type: "connected",
      protocol: { server_name: "test-runtime", version: 5, capabilities: [] },
    }));
    act(() => gateway.emit({ type: "message", message: {
      type: "sessions_list",
      include_archived: false,
      sessions: [{ id: "session-1", label: "Original", workspace: "/workspace", last_seen_secs: 1, archived: false }],
    } }));
    await screen.findByRole("heading", { name: "Original" });

    act(() => screen.getByRole("button", { name: "Session actions" }).click());
    fireEvent.change(screen.getByRole("textbox", { name: "Session label" }), {
      target: { value: "Renamed" },
    });
    act(() => screen.getByRole("button", { name: "Rename" }).click());
    await waitFor(() => expect(gateway.commands.at(-1)).toEqual({
      type: "rename_session", session_id: "session-1", label: "Renamed",
    }));
    expect(screen.getByRole("heading", { name: "Original" })).toBeTruthy();
    act(() => gateway.emit({ type: "message", message: {
      type: "session_updated", session_id: "session-1", label: "Renamed", archived: false,
    } }));
    await screen.findByRole("heading", { name: "Renamed" });

    act(() => screen.getByRole("button", { name: "Session actions" }).click());
    act(() => screen.getByRole("button", { name: "Archive" }).click());
    await waitFor(() => expect(gateway.commands.at(-1)).toEqual({
      type: "archive_session", session_id: "session-1",
    }));
    expect(screen.getByRole("heading", { name: "Renamed" })).toBeTruthy();
    act(() => gateway.emit({ type: "message", message: {
      type: "session_updated", session_id: "session-1", archived: true,
    } }));
    await screen.findByRole("heading", { name: "No Session selected" });

    act(() => gateway.emit({ type: "message", message: {
      type: "sessions_list",
      include_archived: false,
      sessions: [{ id: "session-2", label: "Delete me", workspace: "/workspace", last_seen_secs: 1, archived: false }],
    } }));
    await screen.findByRole("heading", { name: "Delete me" });
    act(() => screen.getByRole("button", { name: "Session actions" }).click());
    act(() => screen.getByRole("button", { name: "Delete permanently" }).click());
    await waitFor(() => expect(gateway.commands.at(-1)).toEqual({
      type: "delete_session", session_id: "session-2",
    }));
    act(() => gateway.emit({ type: "message", message: {
      type: "session_deleted", session_id: "session-2",
    } }));
    await screen.findByRole("heading", { name: "No Session selected" });
  });

  it("restores an archived Session only after Runtime confirms the transition", async () => {
    const gateway = new TestGateway();
    render(<App gateway={gateway} />);
    await waitFor(() => expect(gateway.listener).toBeTypeOf("function"));
    act(() => gateway.emit({
      type: "connected",
      protocol: { server_name: "test-runtime", version: 5, capabilities: [] },
    }));
    act(() => gateway.emit({ type: "message", message: {
      type: "sessions_list",
      include_archived: false,
      sessions: [{ id: "session-1", label: "Recover me", workspace: "/workspace", last_seen_secs: 1, archived: false }],
    } }));
    await screen.findByRole("heading", { name: "Recover me" });
    act(() => gateway.emit({ type: "message", message: {
      type: "session_updated", session_id: "session-1", archived: true,
    } }));
    await screen.findByRole("heading", { name: "No Session selected" });

    act(() => screen.getByRole("button", { name: "Archived · 0" }).click());
    await waitFor(() => expect(gateway.commands.at(-1)).toEqual({
      type: "list_sessions", include_archived: true,
    }));
    act(() => gateway.emit({ type: "message", message: {
      type: "sessions_list",
      include_archived: true,
      sessions: [{ id: "session-1", label: "Recover me", workspace: "/workspace", last_seen_secs: 2, archived: true }],
    } }));
    act(() => screen.getByRole("button", { name: "Restore" }).click());
    await waitFor(() => expect(gateway.commands.at(-1)).toEqual({
      type: "restore_session", session_id: "session-1",
    }));
    expect(screen.getByRole("heading", { name: "No Session selected" })).toBeTruthy();

    act(() => gateway.emit({ type: "message", message: {
      type: "session_updated", session_id: "session-1", archived: false,
    } }));
    await waitFor(() => expect(gateway.commands.slice(-2)).toEqual([
      { type: "list_sessions", include_archived: false },
      { type: "list_sessions", include_archived: true },
    ]));
    act(() => gateway.emit({ type: "message", message: {
      type: "sessions_list",
      include_archived: false,
      sessions: [{ id: "session-1", label: "Recover me", workspace: "/workspace", last_seen_secs: 0, archived: false }],
    } }));
    await screen.findByRole("heading", { name: "Recover me" });
  });

  it("switches to a checkpoint branch only after Runtime returns its history", async () => {
    const gateway = new TestGateway();
    render(<App gateway={gateway} />);
    await waitFor(() => expect(gateway.listener).toBeTypeOf("function"));
    act(() => gateway.emit({
      type: "connected",
      protocol: { server_name: "test-runtime", version: 5, capabilities: [] },
    }));
    act(() => gateway.emit({ type: "message", message: {
      type: "sessions_list",
      include_archived: false,
      sessions: [{ id: "session-1", label: "Original", workspace: "/workspace", last_seen_secs: 1, archived: false }],
    } }));
    await screen.findByRole("heading", { name: "Original" });

    act(() => screen.getByRole("button", { name: "Session actions" }).click());
    act(() => screen.getByRole("button", { name: "Create checkpoint branch" }).click());
    await waitFor(() => expect(gateway.commands.at(-1)).toEqual({
      type: "fork_session",
      session_id: "session-1",
      checkpoint: true,
    }));
    expect(screen.getByRole("heading", { name: "Original" })).toBeTruthy();

    act(() => gateway.emit({ type: "message", message: {
      type: "session_history",
      session: {
        id: "session-2",
        label: "Original checkpoint",
        workspace: "/workspace",
        last_seen_secs: 0,
        archived: false,
      },
      messages: [{ role: "user", text: "preserved" }],
      source_session_id: "session-1",
      notice: "Conversation checkpoint branch created",
    } }));

    await screen.findByRole("heading", { name: "Original checkpoint" });
    expect(screen.getByText("preserved")).toBeTruthy();
    expect(screen.getByText("Conversation checkpoint branch created")).toBeTruthy();
    expect(gateway.commands.at(-1)).toEqual({ type: "list_sessions", include_archived: false });
  });

  it("locks duplicate chat submission until Runtime settles admission", async () => {
    const gateway = new TestGateway();
    render(<App gateway={gateway} />);
    await waitFor(() => expect(gateway.listener).toBeTypeOf("function"));
    act(() => gateway.emit({
      type: "connected",
      protocol: { server_name: "test-runtime", version: 5, capabilities: [] },
    }));
    act(() => gateway.emit({ type: "message", message: {
      type: "sessions_list",
      include_archived: false,
      sessions: [{ id: "session-1", label: "Chat", workspace: "/workspace", last_seen_secs: 1, archived: false }],
    } }));
    const composer = await screen.findByRole("textbox", { name: "Message Sylvander" });
    fireEvent.change(composer, { target: { value: "Run once" } });
    const sendButton = screen.getByRole("button", { name: "Send" });
    act(() => {
      sendButton.click();
      sendButton.click();
    });
    await waitFor(() => expect(gateway.commands.filter((command) => command.type === "chat")).toEqual([
      { type: "chat", text: "Run once", attachments: [], session_id: "session-1" },
    ]));
    expect(composer.hasAttribute("disabled")).toBe(true);

    act(() => gateway.emit({ type: "message", message: {
      type: "operation_error", operation: "chat", message: "admission rejected",
    } }));
    await waitFor(() => expect(composer.hasAttribute("disabled")).toBe(false));
    fireEvent.change(composer, { target: { value: "Retry once" } });
    act(() => screen.getByRole("button", { name: "Send" }).click());
    await waitFor(() => expect(gateway.commands.filter((command) => command.type === "chat")).toHaveLength(2));

    act(() => gateway.emit({ type: "message", message: {
      type: "done", session_id: "session-1", text: "Complete",
    } }));
    await waitFor(() => expect(composer.hasAttribute("disabled")).toBe(false));
  });

  it("submits private feedback only through the Runtime-issued target", async () => {
    const gateway = new TestGateway();
    render(<App gateway={gateway} />);
    await waitFor(() => expect(gateway.listener).toBeTypeOf("function"));
    act(() => gateway.emit({
      type: "connected",
      protocol: { server_name: "test-runtime", version: 6, capabilities: ["feedback_v1"] },
    }));
    act(() => gateway.emit({ type: "message", message: {
      type: "sessions_list",
      include_archived: false,
      sessions: [{ id: "session-1", label: "Feedback", workspace: "/workspace", last_seen_secs: 1, archived: false }],
    } }));
    act(() => gateway.emit({ type: "message", message: {
      type: "done",
      session_id: "session-1",
      text: "Complete",
      feedback_target: "sha256:server-issued-target",
    } }));

    const note = await screen.findByRole("textbox", { name: "Feedback note" });
    fireEvent.change(note, { target: { value: "Clear and correct" } });
    act(() => screen.getByRole("button", { name: "Useful" }).click());
    await waitFor(() => expect(gateway.commands.at(-1)).toEqual({
      type: "submit_feedback",
      feedback: {
        target: "sha256:server-issued-target",
        rating: "positive",
        note: "Clear and correct",
        tags: [],
        artifacts: [],
        validations: [],
        privacy_class: "private",
      },
    }));
    expect(screen.getByRole("button", { name: "Useful" }).hasAttribute("disabled")).toBe(true);

    act(() => gateway.emit({ type: "message", message: {
      type: "feedback_recorded", feedback_id: "feedback-1",
    } }));
    expect(await screen.findByText("Feedback recorded.")).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Useful" })).toBeNull();
  });

  it("settles governed memory only from revision-bound Runtime responses", async () => {
    const gateway = new TestGateway();
    render(<App gateway={gateway} />);
    await waitFor(() => expect(gateway.listener).toBeTypeOf("function"));
    act(() => gateway.emit({
      type: "connected",
      protocol: {
        server_name: "test-runtime",
        version: 6,
        capabilities: ["memory_confirmation_v1"],
      },
    }));
    act(() => gateway.emit({ type: "message", message: {
      type: "sessions_list",
      include_archived: false,
      sessions: [{
        id: "session-1",
        label: "Memory",
        workspace: "/workspace",
        last_seen_secs: 1,
        archived: false,
      }],
    } }));
    await waitFor(() => expect(gateway.commands.at(-1)).toEqual({
      type: "load_session",
      session_id: "session-1",
    }));

    act(() => gateway.emit({ type: "message", message: {
      type: "session_history",
      session: {
        id: "session-1",
        label: "Memory",
        workspace: "/workspace",
        last_seen_secs: 1,
        archived: false,
      },
      messages: [],
    } }));
    await waitFor(() => expect(gateway.commands.at(-1)).toEqual({
      type: "memory_confirmation",
      request: { operation: "list", version: 1, session_id: "session-1" },
    }));

    act(() => gateway.emit({ type: "message", message: {
      type: "memory_confirmation",
      response: {
        result: "pending",
        version: 1,
        session_id: "session-1",
        confirmations: [
          {
            candidate_id: "candidate-1",
            expected_revision: 7,
            scope: "user_profile",
            summary: "Prefers concise release notes",
          },
          {
            candidate_id: "candidate-2",
            expected_revision: 3,
            scope: "workspace_knowledge",
            summary: "Builds release artifacts in CI",
          },
        ],
      },
    } }));
    expect(await screen.findByText("Prefers concise release notes")).toBeTruthy();
    expect(screen.getByText(/Memory confirmation · your profile/)).toBeTruthy();

    act(() => screen.getByRole("button", { name: "Save memory" }).click());
    await waitFor(() => expect(gateway.commands.at(-1)).toEqual({
      type: "memory_confirmation",
      request: {
        operation: "decide",
        version: 1,
        session_id: "session-1",
        candidate_id: "candidate-1",
        expected_revision: 7,
        decision: "confirm",
      },
    }));
    expect(screen.getByText("Prefers concise release notes")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Save memory" }).hasAttribute("disabled")).toBe(true);

    act(() => gateway.emit({ type: "message", message: {
      type: "memory_confirmation",
      response: {
        result: "recorded",
        version: 1,
        session_id: "session-1",
        candidate_id: "candidate-1",
        decision: "confirm",
      },
    } }));
    expect(await screen.findByText("Builds release artifacts in CI")).toBeTruthy();
    expect(screen.queryByText("Prefers concise release notes")).toBeNull();

    act(() => screen.getByRole("button", { name: "Do not save" }).click());
    await waitFor(() => expect(gateway.commands.at(-1)).toMatchObject({
      type: "memory_confirmation",
      request: { operation: "decide", candidate_id: "candidate-2", decision: "reject" },
    }));
    act(() => gateway.emit({ type: "message", message: {
      type: "memory_confirmation",
      response: {
        result: "error",
        version: 1,
        operation: "decide",
        code: "conflict",
        message: "candidate revision changed",
      },
    } }));
    expect(await screen.findByText("Builds release artifacts in CI")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Do not save" }).hasAttribute("disabled")).toBe(false);
    expect(screen.getByText(/memory confirmation failed · candidate revision changed/)).toBeTruthy();
    await waitFor(() => expect(gateway.commands.at(-1)).toEqual({
      type: "memory_confirmation",
      request: { operation: "list", version: 1, session_id: "session-1" },
    }));
  });

  it("edits the authenticated owner's typed profile without raw JSON", async () => {
    const gateway = new TestGateway();
    render(<App gateway={gateway} />);
    await waitFor(() => expect(gateway.listener).toBeTypeOf("function"));
    act(() => gateway.emit({
      type: "connected",
      protocol: { server_name: "test-runtime", version: 6, capabilities: ["user_profile_v1"] },
    }));

    act(() => screen.getByRole("button", { name: "Account settings" }).click());
    await waitFor(() => expect(gateway.commands.at(-1)).toEqual({
      type: "user_profile",
      request: { version: 1, action: { operation: "read" } },
    }));
    act(() => gateway.emit({ type: "message", message: {
      type: "user_profile",
      response: { result: "not_found", version: 1 },
    } }));

    const language = await screen.findByRole("textbox", { name: "Preferred language" });
    fireEvent.change(language, { target: { value: "zh-CN" } });
    fireEvent.change(screen.getByRole("combobox", { name: "Language privacy" }), {
      target: { value: "restricted" },
    });
    fireEvent.change(screen.getByRole("combobox", { name: "Response detail" }), {
      target: { value: "concise" },
    });
    act(() => screen.getByRole("button", { name: "Add constraint" }).click());
    fireEvent.change(screen.getByRole("textbox", { name: "Constraint 1" }), {
      target: { value: "Never expose secrets" },
    });
    act(() => screen.getByRole("button", { name: "Create profile" }).click());
    const createdData = {
      preferred_language: { value: "zh-CN", privacy_class: "restricted" as const },
      response_detail: { value: "concise" as const, privacy_class: "personal" as const },
      constraints: [{ value: "Never expose secrets", privacy_class: "sensitive" as const }],
    };
    await waitFor(() => expect(gateway.commands.at(-1)).toEqual({
      type: "user_profile",
      request: { version: 1, action: { operation: "create", profile: createdData } },
    }));

    act(() => gateway.emit({ type: "message", message: {
      type: "user_profile",
      response: {
        result: "created",
        version: 1,
        profile: {
          revision: 1,
          profile: createdData,
          do_not_learn: false,
          created_at_unix_secs: 10,
          updated_at_unix_secs: 10,
        },
      },
    } }));
    fireEvent.change(await screen.findByRole("textbox", { name: "Preferred language" }), {
      target: { value: "zh-TW" },
    });
    act(() => screen.getByRole("button", { name: "Save profile" }).click());
    await waitFor(() => expect(gateway.commands.at(-1)).toMatchObject({
      type: "user_profile",
      request: {
        action: {
          operation: "update",
          expected_revision: 1,
          profile: { preferred_language: { value: "zh-TW", privacy_class: "restricted" } },
        },
      },
    }));

    const updatedData = {
      ...createdData,
      preferred_language: { value: "zh-TW", privacy_class: "restricted" as const },
    };
    act(() => gateway.emit({ type: "message", message: {
      type: "user_profile",
      response: {
        result: "updated",
        version: 1,
        profile: {
          revision: 2,
          profile: updatedData,
          do_not_learn: false,
          created_at_unix_secs: 10,
          updated_at_unix_secs: 20,
        },
      },
    } }));
    act(() => screen.getByRole("button", { name: "Do not learn" }).click());
    await waitFor(() => expect(gateway.commands.at(-1)).toEqual({
      type: "user_profile",
      request: {
        version: 1,
        action: { operation: "set_do_not_learn", expected_revision: 2, enabled: true },
      },
    }));

    act(() => gateway.emit({ type: "message", message: {
      type: "user_profile",
      response: {
        result: "do_not_learn_updated",
        version: 1,
        profile: {
          revision: 3,
          profile: updatedData,
          do_not_learn: true,
          created_at_unix_secs: 10,
          updated_at_unix_secs: 30,
        },
      },
    } }));
    expect(await screen.findByRole("button", { name: "Allow learning" })).toBeTruthy();
    act(() => screen.getByRole("button", { name: "Prepare JSON export" }).click());
    await waitFor(() => expect(gateway.commands.at(-1)).toEqual({
      type: "user_profile",
      request: { version: 1, action: { operation: "export", format: "json" } },
    }));
    act(() => gateway.emit({ type: "message", message: {
      type: "user_profile",
      response: {
        result: "exported",
        version: 1,
        export: {
          schema_version: 1,
          format: "json",
          profile: {
            revision: 3,
            profile: updatedData,
            do_not_learn: true,
            created_at_unix_secs: 10,
            updated_at_unix_secs: 30,
          },
          exported_at_unix_secs: 40,
        },
      },
    } }));
    expect(await screen.findByRole("button", { name: "Download JSON export" })).toBeTruthy();

    act(() => screen.getByRole("button", { name: "Delete profile…" }).click());
    act(() => screen.getByRole("button", { name: "Confirm profile deletion" }).click());
    await waitFor(() => expect(gateway.commands.at(-1)).toEqual({
      type: "user_profile",
      request: { version: 1, action: { operation: "delete", expected_revision: 3 } },
    }));
  });

  it("carries one-time identity proofs only through the dedicated account surface", async () => {
    const gateway = new TestGateway();
    render(<App gateway={gateway} />);
    await waitFor(() => expect(gateway.listener).toBeTypeOf("function"));
    act(() => gateway.emit({
      type: "connected",
      protocol: {
        server_name: "test-runtime",
        version: 6,
        capabilities: ["user_profile_v1", "identity_binding_v1"],
      },
    }));
    act(() => screen.getByRole("button", { name: "Account settings" }).click());
    act(() => screen.getByRole("tab", { name: "identity" }).click());
    await waitFor(() => expect(gateway.commands.at(-1)).toEqual({
      type: "identity_binding",
      request: { version: 1, action: { operation: "resolve" } },
    }));
    act(() => gateway.emit({ type: "message", message: {
      type: "identity_binding",
      response: { result: "not_linked", version: 1 },
    } }));

    act(() => screen.getByRole("button", { name: "Link an external Channel" }).click());
    await waitFor(() => expect(gateway.commands.at(-1)).toEqual({
      type: "identity_binding",
      request: { version: 1, action: { operation: "begin" } },
    }));
    act(() => gateway.emit({ type: "message", message: {
      type: "identity_binding",
      response: {
        result: "challenge_issued",
        version: 1,
        challenge_id: "challenge-1",
        secret: "one-time-secret-123",
        expires_at_unix_secs: Math.floor(Date.now() / 1_000) + 300,
      },
    } }));
    expect(await screen.findByDisplayValue("challenge-1")).toBeTruthy();
    expect(screen.getByDisplayValue("one-time-secret-123")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Copy one-time proof" })).toBeTruthy();

    fireEvent.change(screen.getByRole("textbox", { name: "Challenge to confirm" }), {
      target: { value: "external-challenge" },
    });
    fireEvent.change(screen.getByRole("textbox", { name: "One-time proof to confirm" }), {
      target: { value: "external-proof-1234" },
    });
    act(() => screen.getByRole("button", { name: "Confirm identity link" }).click());
    await waitFor(() => expect(gateway.commands.at(-1)).toEqual({
      type: "identity_binding",
      request: {
        version: 1,
        action: {
          operation: "confirm",
          challenge_id: "external-challenge",
          proof: "external-proof-1234",
        },
      },
    }));
    expect(screen.queryByDisplayValue("one-time-secret-123")).toBeNull();

    act(() => gateway.emit({ type: "message", message: {
      type: "identity_binding",
      response: {
        result: "resolved",
        version: 1,
        binding: { user_id: "alice", revision: 7, linked_at_unix_secs: 100 },
      },
    } }));
    expect(await screen.findByRole("heading", { name: "Linked as alice" })).toBeTruthy();
    act(() => screen.getByRole("button", { name: "Unlink this ingress…" }).click());
    act(() => screen.getByRole("button", { name: "Confirm unlink revision 7" }).click());
    await waitFor(() => expect(gateway.commands.at(-1)).toEqual({
      type: "identity_binding",
      request: { version: 1, action: { operation: "unlink", expected_revision: 7 } },
    }));
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
    expect(await screen.findByRole("heading", { name: "Linked as alice" })).toBeTruthy();
    expect(screen.getByText("identity binding revision changed")).toBeTruthy();
  });

  it("activates only an explicitly confirmed redacted Agent revision", async () => {
    const gateway = new TestGateway();
    render(<App gateway={gateway} />);
    await waitFor(() => expect(gateway.listener).toBeTypeOf("function"));
    act(() => gateway.emit({
      type: "connected",
      protocol: { server_name: "test-runtime", version: 6, capabilities: ["agent_administration"] },
    }));
    act(() => gateway.emit({ type: "message", message: {
      type: "agents_discovered",
      agents: [{
        id: "agent-1",
        revision: 4,
        name: "Coding Agent",
        provider_id: "openai",
        default_model_id: "gpt-test",
      }],
    } }));
    act(() => screen.getByRole("button", { name: "Agents" }).click());
    await waitFor(() => expect(gateway.commands.at(-1)).toEqual({
      type: "agent_admin",
      request: { operation: "list_revisions", agent_id: "agent-1", limit: 50 },
    }));
    const definition = {
      agent_id: "agent-1",
      revision: 4,
      name: "Coding Agent",
      description: "Works on code",
      provider_id: "openai",
      default_model_id: "gpt-test",
      allowed_models: [{ provider_id: "openai", model_id: "gpt-test" }],
      system_prompt_sha256: "sha256:prompt-4",
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
    };
    act(() => gateway.emit({ type: "message", message: {
      type: "agent_admin",
      response: {
        status: "success",
        result: {
          operation: "revisions_listed",
          agent_id: "agent-1",
          active_revision: 4,
          revisions: [{
            definition,
            digest_sha256: "sha256:definition-4",
            created_at_unix_secs: 100,
            active: true,
          }, {
            definition: { ...definition, revision: 5, system_prompt_sha256: "sha256:prompt-5" },
            digest_sha256: "sha256:definition-5",
            created_at_unix_secs: 200,
            active: false,
          }],
        },
      },
    } }));
    expect(await screen.findByRole("heading", { name: "Revision 4 · active" })).toBeTruthy();
    expect(screen.getByText(/definition sha256:definition-5 · prompt sha256:prompt-5/)).toBeTruthy();
    act(() => screen.getByRole("button", { name: "Make active…" }).click());
    expect(gateway.commands.at(-1)).toEqual({
      type: "agent_admin",
      request: { operation: "list_revisions", agent_id: "agent-1", limit: 50 },
    });
    act(() => screen.getByRole("button", { name: "Confirm activation from revision 4" }).click());
    await waitFor(() => expect(gateway.commands.at(-1)).toEqual({
      type: "agent_admin",
      request: {
        operation: "activate_revision",
        agent_id: "agent-1",
        revision: 5,
        expected_active_revision: 4,
      },
    }));
    expect(screen.getByRole("heading", { name: "Revision 4 · active" })).toBeTruthy();
    act(() => gateway.emit({ type: "message", message: {
      type: "agent_admin",
      response: {
        status: "success",
        result: { operation: "revision_activated", agent_id: "agent-1", active_revision: 5 },
      },
    } }));
    expect(await screen.findByRole("heading", { name: "Revision 5 · active" })).toBeTruthy();
  });

  it("attaches UTF-8 files and gates images by provider-qualified model capability", async () => {
    const gateway = new TestGateway();
    render(<App gateway={gateway} />);
    await waitFor(() => expect(gateway.listener).toBeTypeOf("function"));
    act(() => gateway.emit({
      type: "connected",
      protocol: { server_name: "test-runtime", version: 6, capabilities: [] },
    }));
    act(() => gateway.emit({ type: "message", message: {
      type: "agents_discovered",
      agents: [{ id: "agent-1", revision: 1, name: "Agent", provider_id: "alpha", default_model_id: "shared" }],
    } }));
    act(() => gateway.emit({ type: "message", message: {
      type: "runtime_info",
      snapshot: runtimeSnapshot("alpha", false),
    } }));
    act(() => gateway.emit({ type: "message", message: {
      type: "sessions_list",
      include_archived: false,
      sessions: [{ id: "session-1", label: "Attachments", workspace: "/workspace", last_seen_secs: 1, archived: false }],
    } }));
    await screen.findByRole("heading", { name: "Attachments" });
    const input = screen.getByLabelText("Select attachment files") as HTMLInputElement;
    const pngBytes = new Uint8Array([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 1]);
    fireEvent.change(input, { target: { files: [browserFile(pngBytes, "diagram.png", "image/png")] } });
    expect(await screen.findByText("Active model does not support image attachments")).toBeTruthy();

    act(() => gateway.emit({ type: "message", message: {
      type: "runtime_info",
      snapshot: runtimeSnapshot("beta", true),
    } }));
    fireEvent.change(input, { target: { files: [browserFile(pngBytes, "diagram.png", "image/png")] } });
    expect(await screen.findByRole("button", { name: "Remove diagram.png" })).toBeTruthy();
    fireEvent.change(input, { target: { files: [browserFile(new TextEncoder().encode("evidence"), "notes.md", "text/markdown")] } });
    expect(await screen.findByRole("button", { name: "Remove notes.md" })).toBeTruthy();

    act(() => screen.getByRole("button", { name: "Send" }).click());
    await waitFor(() => expect(gateway.commands.at(-1)).toEqual({
      type: "chat",
      text: "",
      session_id: "session-1",
      attachments: [{
        id: "desktop-attachment-2",
        kind: "image",
        name: "diagram.png",
        mime_type: "image/png",
        content: { encoding: "base64", data: "iVBORw0KGgoB" },
        byte_count: 9,
      }, {
        id: "desktop-attachment-3",
        kind: "file",
        name: "notes.md",
        mime_type: "text/markdown",
        content: { encoding: "text", text: "evidence" },
        byte_count: 8,
      }],
    }));
    expect(screen.queryByRole("button", { name: "Remove diagram.png" })).toBeNull();
  });

  it("requests interruption once and waits for the Runtime terminal", async () => {
    const gateway = new TestGateway();
    render(<App gateway={gateway} />);
    await waitFor(() => expect(gateway.listener).toBeTypeOf("function"));
    act(() => gateway.emit({
      type: "connected",
      protocol: { server_name: "test-runtime", version: 5, capabilities: [] },
    }));
    act(() => gateway.emit({ type: "message", message: {
      type: "sessions_list",
      include_archived: false,
      sessions: [{ id: "session-1", label: "Interrupt", workspace: "/workspace", last_seen_secs: 1, archived: false }],
    } }));
    const composer = await screen.findByRole("textbox", { name: "Message Sylvander" });
    fireEvent.change(composer, { target: { value: "Long task" } });
    act(() => screen.getByRole("button", { name: "Send" }).click());
    const stop = await screen.findByRole("button", { name: "Stop" });
    act(() => {
      stop.click();
      stop.click();
    });
    await waitFor(() => expect(gateway.commands.filter((command) => command.type === "interrupt")).toEqual([
      { type: "interrupt", session_id: "session-1" },
    ]));
    expect(stop.hasAttribute("disabled")).toBe(true);

    act(() => gateway.emit({ type: "message", message: {
      type: "turn_interrupted", session_id: "session-1", reason: "Stopped by user",
    } }));
    expect(await screen.findByText("Stopped by user")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Send" })).toBeTruthy();
  });

  it("uses Runtime turn identity as the sole start fact and projects usage", async () => {
    const gateway = new TestGateway();
    render(<App gateway={gateway} />);
    await waitFor(() => expect(gateway.listener).toBeTypeOf("function"));
    act(() => gateway.emit({
      type: "connected",
      protocol: { server_name: "test-runtime", version: 5, capabilities: [] },
    }));
    act(() => gateway.emit({ type: "message", message: {
      type: "sessions_list",
      include_archived: false,
      sessions: [{ id: "session-1", label: "Usage", workspace: "/workspace", last_seen_secs: 1, archived: false }],
    } }));
    act(() => gateway.emit({ type: "message", message: {
      type: "session_history",
      session: { id: "session-1", label: "Usage", workspace: "/workspace", last_seen_secs: 1, archived: false },
      messages: [],
      iterations: 4,
      input_tokens: 400,
      output_tokens: 100,
      cost_nano_usd: 5_000_000,
    } }));
    act(() => gateway.emit({ type: "message", message: {
      type: "iteration_start", session_id: "session-1", iteration: 1,
    } }));
    expect(screen.getByRole("button", { name: "Send" })).toBeTruthy();
    act(() => gateway.emit({ type: "message", message: {
      type: "turn_started", session_id: "session-1", turn_id: "turn-1",
    } }));
    expect(await screen.findByRole("button", { name: "Stop" })).toBeTruthy();
    act(() => gateway.emit({ type: "message", message: {
      type: "iteration_end",
      session_id: "session-1",
      iteration: 1,
      input_tokens: 460,
      output_tokens: 120,
      cost_nano_usd: 6_500_000,
    } }));
    act(() => screen.getByRole("button", { name: /^Plan / }).click());
    expect(screen.getByText(/5 iterations · 580 tokens · \$0\.006500/)).toBeTruthy();
  });

  it("requests and projects Runtime-owned context and compaction", async () => {
    const gateway = new TestGateway();
    render(<App gateway={gateway} />);
    await waitFor(() => expect(gateway.listener).toBeTypeOf("function"));
    act(() => gateway.emit({
      type: "connected",
      protocol: { server_name: "test-runtime", version: 5, capabilities: [] },
    }));
    act(() => gateway.emit({ type: "message", message: {
      type: "sessions_list",
      include_archived: false,
      sessions: [{ id: "session-1", label: "Context", workspace: "/workspace", last_seen_secs: 1, archived: false }],
    } }));
    act(() => screen.getByRole("button", { name: /^Plan / }).click());
    act(() => screen.getByRole("tab", { name: "context" }).click());
    act(() => screen.getByRole("button", { name: "Refresh" }).click());
    await waitFor(() => expect(gateway.commands.at(-1)).toEqual({
      type: "get_context", session_id: "session-1",
    }));
    expect(screen.getByRole("button", { name: "Refresh" }).hasAttribute("disabled")).toBe(true);
    act(() => gateway.emit({ type: "message", message: {
      type: "context_report",
      report: {
        model: "deep-code",
        context_window: 200_000,
        used_tokens: 50_000,
        remaining_tokens: 150_000,
        cache_read_tokens: 40_000,
        cache_write_tokens: 2_000,
        sources: [{ kind: "conversation", label: "conversation messages", items: 8 }],
      },
    } }));
    expect(await screen.findByText("50000 / 200000 tokens · 25%")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Refresh" }).hasAttribute("disabled")).toBe(false);
    expect(screen.getByText("conversation messages · 8")).toBeTruthy();

    act(() => screen.getByRole("button", { name: "Compact" }).click());
    await waitFor(() => expect(gateway.commands.at(-1)).toEqual({
      type: "compact", session_id: "session-1",
    }));
    act(() => gateway.emit({ type: "message", message: {
      type: "compaction_started", session_id: "session-1", automatic: false,
    } }));
    expect(await screen.findByText("Compaction in progress…")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Compact" }).hasAttribute("disabled")).toBe(true);
    act(() => gateway.emit({ type: "message", message: {
      type: "compaction_completed",
      session_id: "session-1",
      report: {
        automatic: false,
        removed_messages: 12,
        condensed_blocks: 3,
        freed_tokens: 4_200,
        summary: "Kept architecture decisions",
      },
    } }));
    expect(await screen.findByText(/12 messages removed · 3 blocks condensed · ~4200 tokens freed/)).toBeTruthy();
    act(() => gateway.emit({ type: "message", message: {
      type: "compaction_failed",
      session_id: "session-1",
      automatic: true,
      reason: "Provider context changed",
    } }));
    expect(await screen.findByText("Compaction failed · Provider context changed")).toBeTruthy();
  });

  it("submits only Runtime-advertised model and permission selections", async () => {
    const gateway = new TestGateway();
    render(<App gateway={gateway} />);
    await waitFor(() => expect(gateway.listener).toBeTypeOf("function"));
    act(() => gateway.emit({
      type: "connected",
      protocol: { server_name: "test-runtime", version: 5, capabilities: [] },
    }));
    act(() => gateway.emit({ type: "message", message: {
      type: "sessions_list",
      include_archived: false,
      sessions: [{ id: "session-1", label: "Settings", workspace: "/workspace", last_seen_secs: 1, archived: false }],
    } }));
    act(() => gateway.emit({ type: "message", message: {
      type: "runtime_info",
      snapshot: {
        agent_id: "agent-1",
        model: { provider_id: "alpha", model_id: "shared" },
        reasoning_effort: "off",
        models: [
          { id: "shared", provider: "alpha", capabilities: 0, capability_names: [], reasoning_efforts: ["off"], lifecycle: { status: "active" } },
          { id: "shared", provider: "beta", capabilities: 0, capability_names: [], reasoning_efforts: ["low", "high"], lifecycle: { status: "active" } },
        ],
        permissions: { file_access: "workspace_write", network_access: "denied", approval_policy: "allow" },
        capabilities: 0,
        approval_enabled: false,
        max_request_bytes: 1_024,
        platform: {},
      },
    } }));
    act(() => screen.getByRole("button", { name: /alpha\/shared/ }).click());
    fireEvent.change(screen.getByRole("combobox", { name: "Runtime model" }), { target: { value: "1" } });
    expect(within(screen.getByRole("combobox", { name: "Reasoning effort" })).queryByRole("option", { name: "off" })).toBeNull();
    fireEvent.change(screen.getByRole("combobox", { name: "Reasoning effort" }), { target: { value: "high" } });
    act(() => screen.getByRole("button", { name: "Apply model" }).click());
    await waitFor(() => expect(gateway.commands.at(-1)).toEqual({
      type: "select_model",
      session_id: "session-1",
      model: { provider_id: "beta", model_id: "shared" },
      reasoning_effort: "high",
    }));

    act(() => screen.getByRole("button", { name: "Runtime details" }).click());
    expect(within(screen.getByRole("combobox", { name: "Approval policy" })).queryByRole("option", { name: "ask" })).toBeNull();
    fireEvent.change(screen.getByRole("combobox", { name: "File access" }), { target: { value: "read_only" } });
    fireEvent.change(screen.getByRole("combobox", { name: "Network access" }), { target: { value: "allowed" } });
    fireEvent.change(screen.getByRole("combobox", { name: "Approval policy" }), { target: { value: "deny" } });
    act(() => screen.getByRole("button", { name: "Apply permissions" }).click());
    await waitFor(() => expect(gateway.commands.at(-1)).toEqual({
      type: "select_permissions",
      session_id: "session-1",
      profile: { file_access: "read_only", network_access: "allowed", approval_policy: "deny" },
    }));

    act(() => screen.getByRole("button", { name: "Runtime details" }).click());
    await waitFor(() => expect(gateway.commands.at(-1)).toEqual({
      type: "get_session_config", session_id: "session-1",
    }));
    act(() => gateway.emit({ type: "message", message: {
      type: "session_config",
      state: {
        session_id: "session-1",
        revision: 7,
        overrides: {},
        effective: {
          provider_id: "alpha",
          model_id: "shared",
          reasoning_effort: "off",
          permissions: { file_access: "workspace_write", network_access: "denied", approval_policy: "allow" },
          provenance: {
            model: { kind: "agent_default" },
            reasoning_effort: { kind: "agent_default" },
            permissions: { kind: "channel_default" },
          },
        },
      },
    } }));
    expect(await screen.findByText("Session revision 7 · model agent_default · permissions channel_default")).toBeTruthy();
    act(() => screen.getByRole("button", { name: "Pin effective to Session" }).click());
    await waitFor(() => expect(gateway.commands.at(-1)).toEqual({
      type: "update_session_config",
      request: {
        session_id: "session-1",
        expected_revision: 7,
        patch: {
          model: { operation: "set", value: { provider_id: "alpha", model_id: "shared" } },
          reasoning_effort: { operation: "set", value: "off" },
          permissions: { operation: "set", value: { file_access: "workspace_write", network_access: "denied", approval_policy: "allow" } },
        },
      },
    }));
    act(() => gateway.emit({ type: "message", message: {
      type: "session_config",
      state: {
        session_id: "session-1",
        revision: 8,
        overrides: {
          model: { provider_id: "alpha", model_id: "shared" },
          reasoning_effort: "off",
          permissions: { file_access: "workspace_write", network_access: "denied", approval_policy: "allow" },
        },
        effective: {
          provider_id: "alpha",
          model_id: "shared",
          reasoning_effort: "off",
          permissions: { file_access: "workspace_write", network_access: "denied", approval_policy: "allow" },
          provenance: {
            model: { kind: "session_override" },
            reasoning_effort: { kind: "session_override" },
            permissions: { kind: "session_override" },
          },
        },
      },
    } }));
    act(() => screen.getByRole("button", { name: "Restore inheritance" }).click());
    await waitFor(() => expect(gateway.commands.at(-1)).toEqual({
      type: "update_session_config",
      request: {
        session_id: "session-1",
        expected_revision: 8,
        patch: {
          model: { operation: "inherit" },
          reasoning_effort: { operation: "inherit" },
          permissions: { operation: "inherit" },
        },
      },
    }));
    act(() => screen.getByRole("button", { name: "Check liveness" }).click());
    await waitFor(() => expect(gateway.commands.at(-1)).toEqual({ type: "ping" }));
    expect(screen.getByText("Liveness · checking")).toBeTruthy();
    act(() => gateway.emit({ type: "message", message: { type: "pong" } }));
    expect(await screen.findByText("Liveness · healthy")).toBeTruthy();
  });

  it("persists the explicit background-turn notification preference through the native host", async () => {
    const gateway = new TestGateway();
    const host = new TestHost();
    render(<App gateway={gateway} host={host} />);
    await waitFor(() => expect(gateway.listener).toBeTypeOf("function"));
    act(() => gateway.emit({
      type: "connected",
      protocol: { server_name: "test-runtime", version: 6, capabilities: [] },
    }));
    act(() => gateway.emit({ type: "message", message: {
      type: "agents_discovered",
      agents: [{ id: "agent-1", revision: 1, name: "Agent", provider_id: "alpha", default_model_id: "shared" }],
    } }));
    act(() => gateway.emit({ type: "message", message: {
      type: "runtime_info",
      snapshot: runtimeSnapshot("alpha", false),
    } }));

    act(() => screen.getByRole("button", { name: "Runtime details" }).click());
    const notifications = await screen.findByRole("checkbox", {
      name: "Notify when background turns finish",
    });
    expect((notifications as HTMLInputElement).checked).toBe(false);
    act(() => notifications.click());
    await waitFor(() => expect(host.writes).toEqual([true]));
    expect((notifications as HTMLInputElement).checked).toBe(true);

    host.rejectWrites = true;
    act(() => notifications.click());
    expect((await screen.findByRole("alert")).textContent).toBe("Desktop preferences could not be saved");
    expect((notifications as HTMLInputElement).checked).toBe(true);
  });

  it("reviews, accepts, and discards Runtime coding Sessions", async () => {
    const gateway = new TestGateway();
    render(<App gateway={gateway} />);
    await waitFor(() => expect(gateway.listener).toBeTypeOf("function"));
    act(() => gateway.emit({
      type: "connected",
      protocol: { server_name: "test-runtime", version: 5, capabilities: [] },
    }));
    act(() => gateway.emit({ type: "message", message: {
      type: "sessions_list",
      include_archived: false,
      sessions: [{ id: "session-1", label: "Coding", workspace: "/workspace", last_seen_secs: 1, archived: false }],
    } }));
    act(() => screen.getByRole("button", { name: /^Plan / }).click());
    act(() => screen.getByRole("tab", { name: "changes" }).click());
    act(() => screen.getByRole("button", { name: "Load changes" }).click());
    await waitFor(() => expect(gateway.commands.at(-1)).toEqual({
      type: "inspect_coding_session", session_id: "session-1",
    }));
    act(() => gateway.emit({ type: "message", message: {
      type: "coding_session_diff",
      session_id: "session-1",
      diff: { status: " M src/lib.rs", patch: "diff --git a/src/lib.rs b/src/lib.rs\n+verified" },
    } }));
    expect(await screen.findByText(/git status --short/)).toBeTruthy();
    expect(screen.getByText(/\+verified/)).toBeTruthy();
    act(() => screen.getByRole("button", { name: "Accept" }).click());
    await waitFor(() => expect(gateway.commands.at(-1)).toEqual({
      type: "accept_coding_session", session_id: "session-1",
    }));
    act(() => gateway.emit({ type: "message", message: {
      type: "coding_session_accepted", session_id: "session-1",
    } }));
    expect(await screen.findByText("Reviewed changes merged by Runtime.")).toBeTruthy();

    act(() => gateway.emit({ type: "message", message: {
      type: "coding_session_diff",
      session_id: "session-1",
      diff: { status: "?? scratch.txt", patch: "" },
    } }));
    act(() => gateway.emit({ type: "message", message: {
      type: "coding_session_operation_failed",
      session_id: "session-1",
      operation: "accept",
      reason: "target changed",
    } }));
    expect(await screen.findByText("Coding Session operation failed · accept: target changed")).toBeTruthy();
    act(() => screen.getByRole("button", { name: "Discard Session" }).click());
    await waitFor(() => expect(gateway.commands.at(-1)).toEqual({
      type: "discard_coding_session", session_id: "session-1",
    }));
    act(() => gateway.emit({ type: "message", message: {
      type: "coding_session_discarded", session_id: "session-1",
    } }));
    expect(await screen.findByRole("heading", { name: "No Session selected" })).toBeTruthy();
  });

  it("requires a Runtime rollback preview and preserves its turn identity", async () => {
    const gateway = new TestGateway();
    render(<App gateway={gateway} />);
    await waitFor(() => expect(gateway.listener).toBeTypeOf("function"));
    act(() => gateway.emit({
      type: "connected",
      protocol: { server_name: "test-runtime", version: 5, capabilities: [] },
    }));
    act(() => gateway.emit({ type: "message", message: {
      type: "sessions_list",
      include_archived: false,
      sessions: [{ id: "session-1", label: "Rollback", workspace: "/workspace", last_seen_secs: 1, archived: false }],
    } }));
    act(() => screen.getByRole("button", { name: /^Plan / }).click());
    act(() => screen.getByRole("tab", { name: "changes" }).click());
    expect(screen.queryByRole("button", { name: "Confirm rollback" })).toBeNull();
    act(() => screen.getByRole("button", { name: "Preview rollback" }).click());
    await waitFor(() => expect(gateway.commands.at(-1)).toEqual({
      type: "preview_workspace_rollback", session_id: "session-1",
    }));
    act(() => gateway.emit({ type: "message", message: {
      type: "workspace_rollback_preview",
      session_id: "session-1",
      preview: { turn_id: "turn-authoritative", files: ["src/lib.rs", "Cargo.toml"] },
    } }));
    expect(await screen.findByText("src/lib.rs")).toBeTruthy();
    act(() => screen.getByRole("button", { name: "Confirm rollback" }).click());
    await waitFor(() => expect(gateway.commands.at(-1)).toEqual({
      type: "rollback_workspace",
      session_id: "session-1",
      expected_turn_id: "turn-authoritative",
    }));
    act(() => gateway.emit({ type: "message", message: {
      type: "workspace_rollback_completed",
      session_id: "session-1",
      report: { turn_id: "turn-authoritative", restored: ["src/lib.rs", "Cargo.toml"] },
    } }));
    expect(await screen.findByText("2 files restored · conversation history unchanged.")).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Confirm rollback" })).toBeNull();
    act(() => gateway.emit({ type: "message", message: {
      type: "workspace_rollback_failed", session_id: "session-1", reason: "checkpoint changed",
    } }));
    expect(await screen.findByText("Workspace rollback failed · checkpoint changed")).toBeTruthy();
  });

  it("rolls back the local turn lock when native chat submission fails", async () => {
    const gateway = new TestGateway();
    gateway.rejectChat = true;
    render(<App gateway={gateway} />);
    await waitFor(() => expect(gateway.listener).toBeTypeOf("function"));
    act(() => gateway.emit({
      type: "connected",
      protocol: { server_name: "test-runtime", version: 5, capabilities: [] },
    }));
    act(() => gateway.emit({ type: "message", message: {
      type: "sessions_list",
      include_archived: false,
      sessions: [{ id: "session-1", label: "Chat", workspace: "/workspace", last_seen_secs: 1, archived: false }],
    } }));
    const composer = await screen.findByRole("textbox", { name: "Message Sylvander" });
    fireEvent.change(composer, { target: { value: "Keep this draft" } });
    act(() => screen.getByRole("button", { name: "Send" }).click());

    expect(await screen.findByText("Runtime command queue is unavailable")).toBeTruthy();
    await waitFor(() => expect(composer.hasAttribute("disabled")).toBe(false));
    expect((composer as HTMLTextAreaElement).value).toBe("Keep this draft");
  });

  it("projects typed retry, timeout, operation, and boundary failures", async () => {
    const gateway = new TestGateway();
    render(<App gateway={gateway} />);
    await waitFor(() => expect(gateway.listener).toBeTypeOf("function"));
    act(() => gateway.emit({
      type: "connected",
      protocol: { server_name: "test-runtime", version: 5, capabilities: [] },
    }));
    act(() => gateway.emit({ type: "message", message: {
      type: "sessions_list",
      include_archived: false,
      sessions: [{ id: "session-1", label: "Recovery", workspace: "/workspace", last_seen_secs: 1, archived: false }],
    } }));
    await screen.findByRole("heading", { name: "Recovery" });
    act(() => gateway.emit({ type: "message", message: {
      type: "model_retry",
      session_id: "session-1",
      attempt: 2,
      max_attempts: 4,
      delay_ms: 500,
      reason: "provider asked for backoff",
      cause: "rate_limit",
    } }));
    expect(await screen.findByText(/Rate limited · retry 2\/4 in 500ms/)).toBeTruthy();

    act(() => gateway.emit({ type: "message", message: {
      type: "approval_request",
      session_id: "session-1",
      batch_id: "batch-1",
      tools: [{ call_id: "call-1", tool_name: "Write", input: {} }],
    } }));
    await screen.findByRole("heading", { name: "Allow Write?" });
    act(() => gateway.emit({ type: "message", message: {
      type: "interaction_timeout",
      session_id: "session-1",
      kind: "approval",
      subject_id: "call-123456789",
      timeout_secs: 30,
      recovery: "narrow_scope",
    } }));
    expect(screen.queryByRole("heading", { name: "Allow Write?" })).toBeNull();
    expect(await screen.findByText(/timeout · approval · call-123 · 30s · retry with narrower scope/)).toBeTruthy();

    act(() => {
      gateway.emit({ type: "message", message: {
        type: "operation_error", operation: "load_session", message: "Session is unavailable",
      } });
      gateway.emit({ type: "message", message: {
        type: "boundary_denied",
        error: {
          code: "rate_limited",
          operation: "chat",
          request_id: "request-1",
          message: "Too many requests",
          retry_after_ms: 1_000,
        },
      } });
    });
    expect(await screen.findByText("load_session failed · Session is unavailable")).toBeTruthy();
    expect(await screen.findByText("chat denied · Too many requests · retry after 1000ms")).toBeTruthy();
  });

  it("reconnects with the native gateway after an established link drops", async () => {
    vi.useFakeTimers();
    try {
      const gateway = new TestGateway();
      render(<App gateway={gateway} />);
      await act(async () => Promise.resolve());

      act(() => gateway.emit({
        type: "connected",
        protocol: { server_name: "test-runtime", version: 5, capabilities: [] },
      }));
      act(() => gateway.emit({ type: "message", message: {
        type: "sessions_list",
        include_archived: false,
        sessions: [{ id: "session-1", label: "Recovery", workspace: "/workspace", last_seen_secs: 1, archived: false }],
      } }));
      expect(screen.getByRole("heading", { name: "Recovery" })).toBeTruthy();
      act(() => gateway.emit({ type: "disconnected", reason: "runtime_closed" }));

      expect(screen.getAllByText("Reconnecting")).toHaveLength(2);
      await act(async () => {
        vi.advanceTimersByTime(1_000);
        await Promise.resolve();
      });
      expect(gateway.connects).toBe(2);
      await act(async () => {
        gateway.emit({
          type: "connected",
          protocol: { server_name: "test-runtime", version: 5, capabilities: [] },
        });
        await Promise.resolve();
      });
      expect(gateway.commands.at(-1)).toEqual({
        type: "reattach_session",
        session_id: "session-1",
      });
      act(() => gateway.emit({ type: "message", message: {
        type: "session_history",
        session: { id: "session-1", label: "Recovery", workspace: "/workspace", last_seen_secs: 1, archived: false },
        messages: [{ role: "assistant", text: "Recovered history" }],
        iterations: 3,
        input_tokens: 120,
        output_tokens: 30,
        cost_nano_usd: 2_500_000,
        source_session_id: "source-session",
        recovery: true,
        replay_truncated: true,
        notice: "Some in-flight events were truncated",
      } }));
      expect(screen.getByText("Some in-flight events were truncated")).toBeTruthy();
      expect(screen.getByText("Recovered history")).toBeTruthy();
      act(() => screen.getByRole("button", { name: /^Plan / }).click());
      expect(screen.getByText(/3 iterations · 150 tokens · \$0\.002500 · fork of source-s/)).toBeTruthy();
    } finally {
      vi.useRealTimers();
    }
  });

  it("retains an approval batch until Runtime settles each tool", async () => {
    const gateway = new TestGateway();
    render(<App gateway={gateway} />);
    await waitFor(() => expect(gateway.listener).toBeTypeOf("function"));
    act(() => gateway.emit({
      type: "connected",
      protocol: { server_name: "test-runtime", version: 5, capabilities: [] },
    }));
    act(() => gateway.emit({ type: "message", message: {
      type: "sessions_list",
      include_archived: false,
      sessions: [{ id: "session-1", label: "Approvals", workspace: "/workspace", last_seen_secs: 1, archived: false }],
    } }));
    await waitFor(() => expect(gateway.commands.at(-1)?.type).toBe("load_session"));

    act(() => gateway.emit({ type: "message", message: {
      type: "approval_request",
      session_id: "another-session",
      batch_id: "other-batch",
      tools: [{ call_id: "other-call", tool_name: "Delete", input: {} }],
    } }));
    expect(screen.queryByRole("heading", { name: "Allow Delete?" })).toBeNull();

    act(() => gateway.emit({ type: "message", message: {
      type: "approval_request",
      session_id: "session-1",
      batch_id: "batch-1",
      tools: [
        { call_id: "call-1", tool_name: "Read", input: {} },
        { call_id: "call-2", tool_name: "Write", input: {} },
      ],
      allowed_scopes: ["once", "session"],
    } }));
    expect(await screen.findByRole("heading", { name: "Allow Read?" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Allow once" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Allow for Session" })).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Always allow" })).toBeNull();
    act(() => screen.getByRole("button", { name: "Allow for Session" }).click());
    await waitFor(() => expect(gateway.commands.at(-1)).toEqual({
      type: "approve",
      session_id: "session-1",
      call_id: "call-1",
      approved: true,
      scope: "session",
    }));
    expect(screen.getByRole("heading", { name: "Allow Read?" })).toBeTruthy();

    act(() => gateway.emit({ type: "message", message: {
      type: "tool_call", session_id: "session-1", call_id: "call-1", tool_name: "Read", input: {},
    } }));
    expect(await screen.findByRole("heading", { name: "Allow Write?" })).toBeTruthy();
    act(() => screen.getByRole("button", { name: "Reject" }).click());
    await waitFor(() => expect(gateway.commands.at(-1)).toMatchObject({
      type: "approve", call_id: "call-2", approved: false,
    }));
    act(() => gateway.emit({ type: "message", message: {
      type: "tool_rejected", session_id: "session-1", tool_name: "Write", reason: "Denied",
    } }));
    expect(screen.queryByRole("heading", { name: "Allow Write?" })).toBeNull();
  });

  it("answers Runtime questions with the established multi-select encoding", async () => {
    const gateway = new TestGateway();
    render(<App gateway={gateway} />);
    await waitFor(() => expect(gateway.listener).toBeTypeOf("function"));
    act(() => gateway.emit({
      type: "connected",
      protocol: { server_name: "test-runtime", version: 5, capabilities: [] },
    }));
    act(() => gateway.emit({ type: "message", message: {
      type: "sessions_list",
      include_archived: false,
      sessions: [{ id: "session-1", label: "Questions", workspace: "/workspace", last_seen_secs: 1, archived: false }],
    } }));
    await waitFor(() => expect(gateway.commands.at(-1)?.type).toBe("load_session"));
    act(() => gateway.emit({ type: "message", message: {
      type: "ask_user",
      session_id: "session-1",
      call_id: "ask-1",
      question: "Which constraints apply?",
      options: ["urgent", "feature"],
      multi_select: true,
    } }));

    expect(await screen.findByRole("heading", { name: "Which constraints apply?" })).toBeTruthy();
    act(() => screen.getByRole("checkbox", { name: "urgent" }).click());
    act(() => screen.getByRole("checkbox", { name: "feature" }).click());
    fireEvent.change(screen.getByRole("textbox", { name: "Other answer" }), {
      target: { value: "smaller" },
    });
    act(() => screen.getByRole("button", { name: "Answer" }).click());
    await waitFor(() => expect(gateway.commands.at(-1)).toEqual({
      type: "answer",
      session_id: "session-1",
      call_id: "ask-1",
      answer: "urgent, feature; smaller",
    }));
    expect(screen.queryByRole("heading", { name: "Which constraints apply?" })).toBeNull();
  });

  it("resolves a Runtime-owned plan with its typed identity", async () => {
    const gateway = new TestGateway();
    render(<App gateway={gateway} />);
    await waitFor(() => expect(gateway.listener).toBeTypeOf("function"));
    act(() => gateway.emit({
      type: "connected",
      protocol: { server_name: "test-runtime", version: 5, capabilities: [] },
    }));
    act(() => gateway.emit({ type: "message", message: {
      type: "sessions_list",
      include_archived: false,
      sessions: [{ id: "session-1", label: "Plans", workspace: "/workspace", last_seen_secs: 1, archived: false }],
    } }));
    await waitFor(() => expect(gateway.commands.at(-1)?.type).toBe("load_session"));
    act(() => gateway.emit({ type: "message", message: {
      type: "plan_proposed",
      session_id: "session-1",
      plan_id: "plan-1",
      steps: ["Inspect", "Verify"],
      current: 0,
    } }));

    act(() => screen.getByRole("button", { name: /^Plan / }).click());
    expect(await screen.findByText("Inspect")).toBeTruthy();
    act(() => screen.getByRole("button", { name: "Approve plan" }).click());
    await waitFor(() => expect(gateway.commands.at(-1)).toEqual({
      type: "resolve_plan",
      session_id: "session-1",
      plan_id: "plan-1",
      decision: { decision: "approved" },
    }));
    expect(screen.queryByRole("button", { name: "Approve plan" })).toBeNull();

    act(() => gateway.emit({ type: "message", message: {
      type: "plan_updated",
      session_id: "session-1",
      plan_id: "plan-2",
      steps: ["Inspect", "Verify"],
      current: 0,
    } }));
    const secondStep = await screen.findByRole("textbox", { name: "Step 2" });
    fireEvent.change(secondStep, { target: { value: "Run focused verification" } });
    act(() => screen.getByRole("button", { name: "Submit revision" }).click());
    await waitFor(() => expect(gateway.commands.at(-1)).toEqual({
      type: "resolve_plan",
      session_id: "session-1",
      plan_id: "plan-2",
      decision: {
        decision: "revised",
        steps: ["Inspect", "Run focused verification"],
      },
    }));
  });

  it("projects the complete Runtime task lifecycle by task identity", async () => {
    const gateway = new TestGateway();
    render(<App gateway={gateway} />);

    await waitFor(() => expect(gateway.listener).toBeTypeOf("function"));
    act(() => gateway.emit({
      type: "connected",
      protocol: { server_name: "test-runtime", version: 5, capabilities: [] },
    }));
    act(() => gateway.emit({
      type: "message",
      message: {
        type: "sessions_list",
        include_archived: false,
        sessions: [{ id: "session-1", label: "Tasks", workspace: "/workspace", last_seen_secs: 1, archived: false }],
      },
    }));
    await waitFor(() => expect(gateway.commands.at(-1)).toEqual({
      type: "load_session",
      session_id: "session-1",
    }));

    act(() => {
      gateway.emit({ type: "message", message: {
        type: "task_started", session_id: "session-1", task_id: "task-1", owner: "agent", purpose: "Verify build",
      } });
      gateway.emit({ type: "message", message: {
        type: "task_progress", session_id: "session-1", task_id: "task-1", message: "Compiling",
      } });
      gateway.emit({ type: "message", message: {
        type: "task_started", session_id: "session-1", task_id: "task-1", owner: "agent", purpose: "Verify build",
      } });
      gateway.emit({ type: "message", message: {
        type: "task_completed", session_id: "session-1", task_id: "task-1", summary: "All checks passed",
      } });
      gateway.emit({ type: "message", message: {
        type: "task_started", session_id: "session-1", task_id: "task-2", owner: "agent", purpose: "Inspect failure",
      } });
      gateway.emit({ type: "message", message: {
        type: "task_failed", session_id: "session-1", task_id: "task-2", error: "Verifier failed",
      } });
      gateway.emit({ type: "message", message: {
        type: "task_started", session_id: "session-1", task_id: "task-3", owner: "agent", purpose: "Cancelled work",
      } });
      gateway.emit({ type: "message", message: {
        type: "task_cancelled", session_id: "session-1", task_id: "task-3", reason: "User stopped",
      } });
      gateway.emit({ type: "message", message: {
        type: "task_started", session_id: "session-1", task_id: "task-4", owner: "agent", purpose: "Ongoing work",
      } });
      gateway.emit({ type: "message", message: {
        type: "task_progress", session_id: "session-1", task_id: "task-4", message: "Still running",
      } });
    });

    await screen.findByRole("heading", { name: "Tasks" });
    act(() => screen.getByRole("button", { name: /Plan/ }).click());
    act(() => screen.getByRole("tab", { name: "tasks" }).click());
    expect(await screen.findByText("Verify build")).toBeTruthy();
    expect(screen.getByText("agent · complete · All checks passed")).toBeTruthy();
    expect(screen.getByText("agent · failed · Verifier failed")).toBeTruthy();
    expect(screen.getByText("agent · cancelled · User stopped")).toBeTruthy();
    expect(screen.getByText("agent · running · Still running")).toBeTruthy();
    expect(screen.getAllByText("Verify build")).toHaveLength(1);
    act(() => screen.getByRole("button", { name: "Cancel" }).click());
    await waitFor(() => expect(gateway.commands.at(-1)).toEqual({
      type: "cancel_task",
      session_id: "session-1",
      task_id: "task-4",
    }));
    expect(screen.getByText("agent · running · Still running")).toBeTruthy();
    act(() => gateway.emit({ type: "message", message: {
      type: "task_cancelled", session_id: "session-1", task_id: "task-4", reason: "Cancelled by user",
    } }));
    expect(await screen.findByText("agent · cancelled · Cancelled by user")).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Cancel" })).toBeNull();
  });
});

function runtimeSnapshot(providerId: "alpha" | "beta", vision: boolean) {
  return {
    agent_id: "agent-1",
    model: { provider_id: providerId, model_id: "shared" },
    reasoning_effort: "off" as const,
    models: [{
      id: "shared",
      provider: "alpha",
      capabilities: 0,
      capability_names: [],
      reasoning_efforts: ["off" as const],
      lifecycle: { status: "active" as const },
    }, {
      id: "shared",
      provider: "beta",
      capabilities: 0,
      capability_names: vision ? ["vision"] : [],
      reasoning_efforts: ["off" as const],
      lifecycle: { status: "active" as const },
    }],
    permissions: {
      file_access: "workspace_write" as const,
      network_access: "denied" as const,
      approval_policy: "ask" as const,
    },
    capabilities: 0,
    approval_enabled: true,
    max_request_bytes: 1_000_000,
    platform: {},
  };
}

function browserFile(bytes: Uint8Array, name: string, type: string) {
  const selected = new File([Uint8Array.from(bytes).buffer], name, { type });
  if (typeof selected.arrayBuffer !== "function") {
    Object.defineProperty(selected, "arrayBuffer", {
      value: async () => bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength),
    });
  }
  return selected;
}
