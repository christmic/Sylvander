import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import App from "./App";
import type { DesktopEvent, RuntimeCommand, RuntimeGatewayPort } from "./lib/gateway";

afterEach(cleanup);

class TestGateway implements RuntimeGatewayPort {
  commands: RuntimeCommand[] = [];
  connects = 0;
  listener?: (event: DesktopEvent) => void;

  async connect(listener: (event: DesktopEvent) => void) {
    this.connects += 1;
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
      "list_sessions",
      "get_runtime_info",
    ]));

    act(() => gateway.emit({
      type: "message",
      message: {
        type: "sessions_list",
        sessions: [{ id: "session-1", label: "Long-term desktop", workspace: "/workspace", last_seen_secs: 4 }],
      },
    }));

    await waitFor(() => expect(gateway.commands.at(-1)).toEqual({ type: "load_session", session_id: "session-1" }));
    expect(within(screen.getByLabelText("Sessions")).getByText("Long-term desktop")).toBeTruthy();

    act(() => {
      gateway.emit({ type: "message", message: { type: "text_delta", session_id: "session-1", delta: "Hello " } });
      gateway.emit({ type: "message", message: { type: "text_delta", session_id: "session-1", delta: "world" } });
    });

    expect(await screen.findByText("Hello world")).toBeTruthy();
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
      act(() => gateway.emit({ type: "disconnected", reason: "runtime_closed" }));

      expect(screen.getAllByText("Reconnecting")).toHaveLength(2);
      await act(async () => {
        vi.advanceTimersByTime(1_000);
        await Promise.resolve();
      });
      expect(gateway.connects).toBe(2);
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
    } }));
    expect(await screen.findByRole("heading", { name: "Allow Read?" })).toBeTruthy();
    act(() => screen.getByRole("button", { name: "Allow once" }).click());
    await waitFor(() => expect(gateway.commands.at(-1)).toEqual({
      type: "approve",
      session_id: "session-1",
      call_id: "call-1",
      approved: true,
      scope: "once",
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
