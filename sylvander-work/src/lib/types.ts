export type ConnectionState = "starting" | "connecting" | "live" | "reconnecting" | "offline";
export type SessionState = "active" | "waiting" | "idle" | "failed";
export type TranscriptKind = "user" | "assistant" | "tool" | "notice";

export interface SessionSummary {
  id: string;
  label: string;
  workspace: string;
  recency: string;
  state: SessionState;
  draft: string;
}

export interface TranscriptEntry {
  id: string;
  kind: TranscriptKind;
  title?: string;
  body: string;
  meta?: string;
  status?: "running" | "verified" | "waiting" | "failed";
}

export interface PlanStep {
  label: string;
  state: "complete" | "current" | "pending";
}

export interface TaskSummary {
  owner: string;
  purpose: string;
  state: "running" | "complete" | "pending";
}
