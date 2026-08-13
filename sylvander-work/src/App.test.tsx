import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import App from "./App";
import type { DesktopEvent, RuntimeCommand, RuntimeGatewayPort } from "./lib/gateway";

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
