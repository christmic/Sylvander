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

    await waitFor(() => expect(gateway.commands.map((command) => command.type)).toEqual([
      "discover_agents",
      "list_sessions",
      "get_runtime_info",
    ]));
    act(() => gateway.emit({ type: "message", message: {
      type: "runtime_info",
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
      max_attachment_bytes: 1_024,
      platform: {},
    } }));
    expect(await screen.findByRole("button", { name: /openai\/gpt-test/ })).toBeTruthy();
    expect(screen.getByRole("button", { name: /Medium reasoning/ })).toBeTruthy();
    expect(screen.getByText(/workspace write · network denied · approval ask/)).toBeTruthy();

    act(() => gateway.emit({
      type: "message",
      message: {
        type: "sessions_list",
        sessions: [{ id: "session-1", label: "Long-term desktop", workspace: "/workspace", last_seen_secs: 4 }],
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
      { type: "list_sessions" },
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
      sessions: [{ id: "session-1", label: "Original", workspace: "/workspace", last_seen_secs: 1 }],
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
      sessions: [{ id: "session-2", label: "Delete me", workspace: "/workspace", last_seen_secs: 1 }],
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

  it("locks duplicate chat submission until a Runtime terminal arrives", async () => {
    const gateway = new TestGateway();
    render(<App gateway={gateway} />);
    await waitFor(() => expect(gateway.listener).toBeTypeOf("function"));
    act(() => gateway.emit({
      type: "connected",
      protocol: { server_name: "test-runtime", version: 5, capabilities: [] },
    }));
    act(() => gateway.emit({ type: "message", message: {
      type: "sessions_list",
      sessions: [{ id: "session-1", label: "Chat", workspace: "/workspace", last_seen_secs: 1 }],
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
      type: "done", session_id: "session-1", text: "Complete",
    } }));
    await waitFor(() => expect(composer.hasAttribute("disabled")).toBe(false));
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
      sessions: [{ id: "session-1", label: "Chat", workspace: "/workspace", last_seen_secs: 1 }],
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
      sessions: [{ id: "session-1", label: "Recovery", workspace: "/workspace", last_seen_secs: 1 }],
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
        sessions: [{ id: "session-1", label: "Recovery", workspace: "/workspace", last_seen_secs: 1 }],
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
        session: { id: "session-1", label: "Recovery", workspace: "/workspace", last_seen_secs: 1 },
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
      sessions: [{ id: "session-1", label: "Approvals", workspace: "/workspace", last_seen_secs: 1 }],
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
      sessions: [{ id: "session-1", label: "Questions", workspace: "/workspace", last_seen_secs: 1 }],
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
      sessions: [{ id: "session-1", label: "Plans", workspace: "/workspace", last_seen_secs: 1 }],
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
        sessions: [{ id: "session-1", label: "Tasks", workspace: "/workspace", last_seen_secs: 1 }],
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
