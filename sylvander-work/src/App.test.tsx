import { act, cleanup, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import App from "./App";
import type { DesktopEvent, RuntimeCommand, RuntimeGatewayPort } from "./lib/gateway";

afterEach(cleanup);

class TestGateway implements RuntimeGatewayPort {
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
  });
});
