import type { PlanStep, SessionSummary, TaskSummary, TranscriptEntry } from "./types";

export const demoSessions: SessionSummary[] = [
  {
    id: "desktop-foundation",
    label: "Desktop foundation",
    workspace: "~/OraculoSpace/Sylvander",
    recency: "Now",
    state: "active",
    draft: "",
  },
  {
    id: "runtime-boundary",
    label: "Runtime boundary audit",
    workspace: "~/OraculoSpace/Sylvander",
    recency: "18 min",
    state: "waiting",
    draft: "Review the remaining ownership gaps",
  },
  {
    id: "benchmarks",
    label: "Provider benchmarks",
    workspace: "~/OraculoSpace/Sylvander-llm-agent-bench",
    recency: "2 hr",
    state: "idle",
    draft: "",
  },
  {
    id: "mcp-isolation",
    label: "MCP session isolation",
    workspace: "~/OraculoSpace/Sylvander",
    recency: "Yesterday",
    state: "idle",
    draft: "",
  },
];

export const demoTranscript: TranscriptEntry[] = [
  {
    id: "user-1",
    kind: "user",
    body: "Give Sylvander a high-performance desktop experience with friendly, complete interactions.",
  },
  {
    id: "assistant-1",
    kind: "assistant",
    body: "I’m grounding the desktop in the existing Runtime boundary first. The UI will remain a client: Sessions, authorization, tools, and durable state stay server-owned.",
    meta: "Architecture · 34s",
  },
  {
    id: "tool-1",
    kind: "tool",
    title: "Inspect product boundaries",
    body: "Read ChannelHost, UI protocol, TUI client, and WebSocket adapter",
    meta: "7 files · 1,842 lines",
    status: "verified",
  },
  {
    id: "assistant-2",
    kind: "assistant",
    body: "Tauri 2 and Svelte 5 are the best fit for this phase: a small Rust shell, mature text and accessibility semantics, and a compiled reactive layer for streaming work.",
    meta: "Decision recorded",
  },
  {
    id: "tool-2",
    kind: "tool",
    title: "Build desktop shell",
    body: "Scaffolding navigation, conversation, Composer, and contextual work surfaces",
    meta: "Running now",
    status: "running",
  },
];

export const demoPlan: PlanStep[] = [
  { label: "Audit product and protocol boundaries", state: "complete" },
  { label: "Select the desktop architecture", state: "complete" },
  { label: "Build the interaction shell", state: "current" },
  { label: "Connect the production gateway", state: "pending" },
  { label: "Establish release evidence", state: "pending" },
];

export const demoTasks: TaskSummary[] = [
  { owner: "Oraculo", purpose: "Interaction shell", state: "running" },
  { owner: "Runtime", purpose: "Protocol conformance", state: "pending" },
];
