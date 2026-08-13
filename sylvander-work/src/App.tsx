import { FormEvent, KeyboardEvent, useEffect, useMemo, useState } from "react";

import crabMark from "../../docs/design/final-brand/sylvander-seed-crab-character-square.png";
import type { ApprovalScope, ReasoningEffort, RuntimeGatewayPort, RuntimePermissionProfile, RuntimeSessionConfigPatch } from "./lib/gateway";
import { useRuntime, type RuntimeViewState } from "./lib/useRuntime";

export interface AppProps {
  gateway?: RuntimeGatewayPort;
}

export default function App({ gateway }: AppProps) {
  const { state, selectSession, submit, answerQuestion, resolvePlan, cancelTask, submitFeedback, sendChat, interruptTurn, requestContext, compactContext, checkLiveness } = useRuntime(gateway);
  const [query, setQuery] = useState("");
  const [drafts, setDrafts] = useState<Record<string, string>>({});
  const [inspector, setInspector] = useState<"plan" | "tasks" | "changes" | "context">("plan");
  const [inspectorOpen, setInspectorOpen] = useState(false);
  const [sidebarOpen, setSidebarOpen] = useState(false);
  const [questionSelections, setQuestionSelections] = useState<string[]>([]);
  const [questionText, setQuestionText] = useState("");
  const [feedbackNote, setFeedbackNote] = useState("");
  const [planRevision, setPlanRevision] = useState<string[]>([]);
  const [createOpen, setCreateOpen] = useState(false);
  const [newSessionLabel, setNewSessionLabel] = useState("New session");
  const [newSessionAgentId, setNewSessionAgentId] = useState("");
  const [sessionActionsOpen, setSessionActionsOpen] = useState(false);
  const [archiveOpen, setArchiveOpen] = useState(false);
  const [sessionLabel, setSessionLabel] = useState("");
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [modelIndex, setModelIndex] = useState("0");
  const [reasoningEffort, setReasoningEffort] = useState<ReasoningEffort>("off");
  const [permissionProfile, setPermissionProfile] = useState<RuntimePermissionProfile>({
    file_access: "workspace_write",
    network_access: "denied",
    approval_policy: "allow",
  });
  const [compactLayout, setCompactLayout] = useState(() =>
    typeof matchMedia === "function" && matchMedia("(max-width: 860px)").matches);
  const selected = state.sessions.find((session) => session.id === state.selectedId);
  const draft = state.selectedId ? drafts[state.selectedId] ?? "" : "";
  const visibleSessions = useMemo(() => {
    const needle = query.toLowerCase();
    return state.sessions.filter((session) => `${session.label} ${session.workspace}`.toLowerCase().includes(needle));
  }, [query, state.sessions]);

  useEffect(() => {
    if (typeof matchMedia !== "function") return;
    const query = matchMedia("(max-width: 860px)");
    const update = () => setCompactLayout(query.matches);
    query.addEventListener("change", update);
    update();
    return () => query.removeEventListener("change", update);
  }, []);

  useEffect(() => {
    if (state.activePlan) setPlanRevision(state.plan.map((step) => step.label));
  }, [state.activePlan?.planId, state.plan]);

  useEffect(() => {
    if (!newSessionAgentId && state.agents[0]) setNewSessionAgentId(state.agents[0].id);
  }, [newSessionAgentId, state.agents]);

  useEffect(() => {
    setSessionLabel(selected?.label ?? "");
    setSessionActionsOpen(false);
  }, [selected?.id, selected?.label]);

  useEffect(() => {
    if (!state.runtimeInfo) return;
    const index = state.runtimeInfo.models.findIndex((model) =>
      model.provider === state.runtimeInfo!.providerId && model.id === state.runtimeInfo!.modelId);
    setModelIndex(String(Math.max(index, 0)));
    setReasoningEffort(state.runtimeInfo.reasoningEffort);
    setPermissionProfile({
      file_access: state.runtimeInfo.fileAccess,
      network_access: state.runtimeInfo.networkAccess,
      approval_policy: state.runtimeInfo.approvalPolicy,
    });
  }, [state.runtimeInfo]);

  function updateDraft(value: string) {
    if (state.selectedId) setDrafts((current) => ({ ...current, [state.selectedId!]: value }));
  }

  async function send(event?: FormEvent) {
    event?.preventDefault();
    if (!state.selectedId || state.connection !== "live" || !draft.trim()) return;
    if (await sendChat(state.selectedId, draft.trim())) updateDraft("");
  }

  function handleComposerKey(event: KeyboardEvent<HTMLTextAreaElement>) {
    if (event.key === "Enter" && !event.shiftKey) {
      event.preventDefault();
      void send();
    }
  }

  async function decide(callId: string, approved: boolean, scope: ApprovalScope = "once") {
    if (!state.approval) return;
    await submit({
      type: "approve",
      session_id: state.approval.sessionId,
      call_id: callId,
      approved,
      scope,
    });
  }

  function toggleQuestionOption(option: string) {
    if (!state.question?.multiSelect) {
      setQuestionSelections([option]);
      return;
    }
    setQuestionSelections((current) => current.includes(option)
      ? current.filter((candidate) => candidate !== option)
      : [...current, option]);
  }

  async function submitQuestion(event: FormEvent) {
    event.preventDefault();
    if (!state.question) return;
    const selected = questionSelections.join(", ");
    const detail = questionText.trim();
    const answer = selected && detail ? `${selected}; ${detail}` : selected || detail;
    if (!answer) return;
    await answerQuestion(state.question.callId, answer);
    setQuestionSelections([]);
    setQuestionText("");
  }

  async function recordFeedback(rating: "positive" | "negative") {
    if (await submitFeedback(rating, feedbackNote)) setFeedbackNote("");
  }

  function updatePlanStep(index: number, value: string) {
    setPlanRevision((current) => current.map((step, stepIndex) => stepIndex === index ? value : step));
  }

  async function revisePlan(event: FormEvent) {
    event.preventDefault();
    if (!state.activePlan) return;
    const steps = planRevision.map((step) => step.trim()).filter(Boolean);
    if (steps.length === 0) return;
    await resolvePlan(state.activePlan.planId, { decision: "revised", steps });
  }

  async function createSession(event: FormEvent) {
    event.preventDefault();
    const label = newSessionLabel.trim();
    if (!label || !newSessionAgentId) return;
    await submit({
      type: "create_session",
      request: {
        agent_id: newSessionAgentId,
        label,
        overrides: {},
      },
    });
    setCreateOpen(false);
  }

  async function renameSession(event: FormEvent) {
    event.preventDefault();
    const label = sessionLabel.trim();
    if (!selected || !label) return;
    await submit({ type: "rename_session", session_id: selected.id, label });
    setSessionActionsOpen(false);
  }

  async function archiveSession() {
    if (!selected) return;
    await submit({ type: "archive_session", session_id: selected.id });
    setSessionActionsOpen(false);
  }

  function openArchive() {
    setArchiveOpen(true);
    void submit({ type: "list_sessions", include_archived: true });
  }

  async function restoreSession(sessionId: string) {
    await submit({ type: "restore_session", session_id: sessionId });
  }

  async function checkpointSession() {
    if (!selected) return;
    await submit({
      type: "fork_session",
      session_id: selected.id,
      checkpoint: true,
    });
    setSessionActionsOpen(false);
  }

  async function deleteSession() {
    if (!selected) return;
    await submit({ type: "delete_session", session_id: selected.id });
    setSessionActionsOpen(false);
  }

  async function selectRuntimeModel() {
    const model = state.runtimeInfo?.models[Number(modelIndex)];
    if (!model || !model.reasoning_efforts.includes(reasoningEffort)) return;
    await submit({
      type: "select_model",
      ...(selected ? { session_id: selected.id } : {}),
      model: { provider_id: model.provider, model_id: model.id },
      reasoning_effort: reasoningEffort,
    });
    setSettingsOpen(false);
  }

  async function selectRuntimePermissions() {
    await submit({
      type: "select_permissions",
      ...(selected ? { session_id: selected.id } : {}),
      profile: permissionProfile,
    });
    setSettingsOpen(false);
  }

  function openRuntimeSettings() {
    setSettingsOpen(true);
    if (selected) void submit({ type: "get_session_config", session_id: selected.id });
  }

  async function patchSessionConfiguration(operation: "set" | "inherit") {
    const config = state.sessionConfig;
    if (!selected || !config || config.session_id !== selected.id) return;
    const patch: RuntimeSessionConfigPatch = operation === "inherit"
      ? {
          model: { operation: "inherit" },
          reasoning_effort: { operation: "inherit" },
          permissions: { operation: "inherit" },
        }
      : {
          model: {
            operation: "set",
            value: {
              provider_id: config.effective.provider_id,
              model_id: config.effective.model_id,
            },
          },
          reasoning_effort: { operation: "set", value: config.effective.reasoning_effort },
          permissions: { operation: "set", value: config.effective.permissions },
        };
    await submit({
      type: "update_session_config",
      request: {
        session_id: selected.id,
        expected_revision: config.revision,
        patch,
      },
    });
  }

  return <div className="app-shell">
    <nav className="product-rail" aria-label="Product">
      <div className="brand-mark"><img src={crabMark} alt="Sylvander Seed-Crab" /></div>
      <div className="rail-actions">
        <button className="rail-button active" aria-label="Work" aria-current="page" aria-expanded={sidebarOpen} onClick={() => setSidebarOpen(!sidebarOpen)}><span>◫</span></button>
        <button className="rail-button" aria-label="Agents"><span>◎</span></button>
        <button className="rail-button" aria-label="Automations"><span>⌁</span></button>
      </div>
      <button className="rail-button settings" aria-label="Settings"><span>⚙</span></button>
    </nav>

    <aside
      className={`session-sidebar${sidebarOpen ? " open" : ""}`}
      aria-label="Sessions"
      aria-hidden={compactLayout && !sidebarOpen ? true : undefined}
      inert={compactLayout && !sidebarOpen ? true : undefined}
    >
      <header className="sidebar-header">
        <div><span className="eyebrow">Workspace</span><h1>Sylvander Work</h1></div>
        <button className="icon-button" aria-label="Create Session" onClick={() => setCreateOpen(true)} disabled={state.connection !== "live" || state.agents.length === 0}>＋</button>
      </header>
      <label className="session-search" htmlFor="session-search">
        <span aria-hidden="true">⌕</span>
        <input id="session-search" value={query} onChange={(event) => setQuery(event.target.value)} placeholder="Find a Session" autoComplete="off" />
        <kbd>⌘K</kbd>
      </label>
      <div className="session-section-label"><span>Recent</span><button type="button" onClick={openArchive}>Archived · {state.archivedSessions.length}</button></div>
      <div className="session-list">
        {visibleSessions.map((session) => <button
          key={session.id}
          className={`session-row${session.id === state.selectedId ? " active" : ""}`}
          onClick={() => { void selectSession(session.id); setSidebarOpen(false); }}
          aria-current={session.id === state.selectedId ? "true" : undefined}
        >
          <span className={`presence ${session.state}`} />
          <span className="session-copy"><strong>{session.label}</strong><span>{session.workspace}</span></span>
          <time>{session.recency}</time>
        </button>)}
      </div>
      {archiveOpen && <section className="archive-panel" aria-labelledby="archive-title"><header><h2 id="archive-title">Archived Sessions</h2><button type="button" aria-label="Close archive" onClick={() => setArchiveOpen(false)}>×</button></header>{state.archivedSessions.length === 0 ? <p>No archived Sessions.</p> : state.archivedSessions.map((session) => <article key={session.id}><div><strong>{session.label}</strong><span>{session.workspace}</span></div><button type="button" className="secondary-button" onClick={() => void restoreSession(session.id)}>Restore</button></article>)}</section>}
      <footer className="runtime-card">
        <span className={`runtime-dot ${state.connection}`} />
        <div><strong>{state.protocol?.serverName ?? "Local Runtime"}</strong><span>{connectionLabel(state.connection)}{state.runtimeInfo ? ` · ${permissionLabel(state.runtimeInfo)}` : ""}</span></div>
        <button aria-label="Runtime details" onClick={openRuntimeSettings}>···</button>
      </footer>
    </aside>

    <main className="conversation" aria-label="Conversation">
      <header className="conversation-header">
        <div className="session-heading">
          <button className="mobile-nav" aria-label="Open Sessions" aria-expanded={sidebarOpen} onClick={() => setSidebarOpen(!sidebarOpen)}>◫</button>
          <span className={`presence ${selected?.state ?? "idle"}`} />
          <div><h2>{selected?.label ?? "No Session selected"}</h2><p>{selected?.workspace ?? "Connect Runtime to continue"}</p></div>
        </div>
        <div className="header-actions">
          <button className="quiet-button" onClick={() => setInspectorOpen(!inspectorOpen)} aria-pressed={inspectorOpen}>Plan <span>{state.plan.filter((step) => step.state === "complete").length}/{state.plan.length}</span></button>
          <button className="icon-button" aria-label="Session actions" disabled={!selected} onClick={() => setSessionActionsOpen(!sessionActionsOpen)}>···</button>
        </div>
      </header>

      <section className="transcript" aria-label="Transcript">
        {state.connection !== "live" && <div className="session-intro">
          <span className="eyebrow">Rust Runtime</span>
          <h3>{state.connection === "offline" ? "Runtime is unavailable." : "Connecting the desktop workspace…"}</h3>
          <p>{state.diagnostic ?? "The native gateway is negotiating the authenticated UI protocol."}</p>
        </div>}
        {state.connection === "live" && !selected && <div className="session-intro">
          <span className="eyebrow">Agent workspace</span>
          <h3>Start with a durable Session.</h3>
          <p>Sessions, authorization, tools, and evidence remain Runtime-owned.</p>
        </div>}
        {state.transcript.map((entry) => <article key={entry.id} className={`turn ${entry.kind}`} data-status={entry.status}>
          <span className="turn-mark" aria-hidden="true">{entry.kind === "user" ? "❯" : entry.kind === "tool" ? "⎿" : "⏺"}</span>
          <div className="turn-content">
            {entry.title && <strong className="turn-title">{entry.title}</strong>}
            <p>{entry.body}</p>
            {entry.meta && <span className="turn-meta">{entry.meta}</span>}
          </div>
          {entry.kind === "tool" && <button className="inspect-button" onClick={() => setInspectorOpen(true)}>Inspect</button>}
        </article>)}
      </section>

      <div className="interaction-zone">
        {sessionActionsOpen && selected && <form className="decision-dock" aria-labelledby="session-actions-title" onSubmit={(event) => void renameSession(event)}><div className="decision-icon" aria-hidden="true">···</div><div className="decision-copy"><span className="eyebrow">Runtime Session</span><h3 id="session-actions-title">Manage {selected.label}</h3><label>Name<input aria-label="Session label" value={sessionLabel} onChange={(event) => setSessionLabel(event.target.value)} /></label><p>A checkpoint branches conversation history without changing the source Session or workspace files. Archive hides active work; delete is permanent.</p></div><div className="decision-actions"><button type="button" className="secondary-button" onClick={() => void checkpointSession()}>Create checkpoint branch</button><button type="button" className="secondary-button" onClick={() => void archiveSession()}>Archive</button><button type="button" className="secondary-button" onClick={() => void deleteSession()}>Delete permanently</button><button className="primary-button" disabled={!sessionLabel.trim()}>Rename</button></div></form>}
        {createOpen && <form className="decision-dock" aria-labelledby="create-session-title" onSubmit={(event) => void createSession(event)}><div className="decision-icon" aria-hidden="true">＋</div><div className="decision-copy"><span className="eyebrow">Runtime Session</span><h3 id="create-session-title">Create Session</h3><label>Name<input aria-label="Session name" value={newSessionLabel} onChange={(event) => setNewSessionLabel(event.target.value)} /></label><label>Agent<select aria-label="Session Agent" value={newSessionAgentId} onChange={(event) => setNewSessionAgentId(event.target.value)}>{state.agents.map((agent) => <option key={agent.id} value={agent.id}>{agent.name} · {agent.providerId}/{agent.modelId}</option>)}</select></label></div><div className="decision-actions"><button type="button" className="secondary-button" onClick={() => setCreateOpen(false)}>Cancel</button><button className="primary-button" disabled={!newSessionLabel.trim() || !newSessionAgentId}>Create</button></div></form>}
        {settingsOpen && state.runtimeInfo && <section className="decision-dock" aria-labelledby="runtime-settings-title"><div className="decision-icon" aria-hidden="true">⚙</div><div className="decision-copy"><span className="eyebrow">Runtime validated</span><h3 id="runtime-settings-title">Model and permissions</h3><label>Model<select aria-label="Runtime model" value={modelIndex} onChange={(event) => {
          const nextIndex = event.target.value;
          const efforts = state.runtimeInfo!.models[Number(nextIndex)]?.reasoning_efforts ?? [];
          setModelIndex(nextIndex);
          if (!efforts.includes(reasoningEffort)) setReasoningEffort(efforts[0] ?? "off");
        }}>{state.runtimeInfo.models.map((model, index) => <option key={`${model.provider}/${model.id}`} value={index}>{model.provider}/{model.id}</option>)}</select></label><label>Reasoning<select aria-label="Reasoning effort" value={reasoningEffort} onChange={(event) => setReasoningEffort(event.target.value as ReasoningEffort)}>{(state.runtimeInfo.models[Number(modelIndex)]?.reasoning_efforts ?? []).map((effort) => <option key={effort} value={effort}>{effort}</option>)}</select></label><label>Files<select aria-label="File access" value={permissionProfile.file_access} onChange={(event) => setPermissionProfile((current) => ({ ...current, file_access: event.target.value as RuntimePermissionProfile["file_access"] }))}><option value="none">none</option><option value="read_only">read only</option><option value="workspace_write">workspace write</option></select></label><label>Network<select aria-label="Network access" value={permissionProfile.network_access} onChange={(event) => setPermissionProfile((current) => ({ ...current, network_access: event.target.value as RuntimePermissionProfile["network_access"] }))}><option value="denied">denied</option><option value="allowed">allowed</option></select></label><label>Approval<select aria-label="Approval policy" value={permissionProfile.approval_policy} onChange={(event) => setPermissionProfile((current) => ({ ...current, approval_policy: event.target.value as RuntimePermissionProfile["approval_policy"] }))}>{state.runtimeInfo.approvalEnabled && <option value="ask">ask</option>}<option value="allow">allow</option><option value="deny">deny</option></select></label>{selected && (state.sessionConfig ? <p>Session revision {state.sessionConfig.revision} · model {state.sessionConfig.effective.provenance.model.kind} · permissions {state.sessionConfig.effective.provenance.permissions.kind}</p> : <p>Loading Session configuration…</p>)}<p role="status">Liveness · {state.liveness}</p></div><div className="decision-actions"><button className="secondary-button" onClick={() => setSettingsOpen(false)}>Cancel</button><button className="primary-button" disabled={!state.runtimeInfo.models[Number(modelIndex)]} onClick={() => void selectRuntimeModel()}>Apply model</button><button className="primary-button" onClick={() => void selectRuntimePermissions()}>Apply permissions</button>{selected && <><button className="secondary-button" disabled={!state.sessionConfig} onClick={() => void patchSessionConfiguration("set")}>Pin effective to Session</button><button className="secondary-button" disabled={!state.sessionConfig} onClick={() => void patchSessionConfiguration("inherit")}>Restore inheritance</button></>}<button className="secondary-button" disabled={state.connection !== "live" || state.liveness === "checking"} onClick={() => void checkLiveness()}>Check liveness</button></div></section>}
        {state.question && <form className="decision-dock" aria-labelledby="question-title" onSubmit={(event) => void submitQuestion(event)}>
          <div className="decision-icon" aria-hidden="true">?</div>
          <div className="decision-copy"><span className="eyebrow">Agent asks</span><h3 id="question-title">{state.question.prompt}</h3><div className="question-options">{state.question.options.map((option) => <label key={option}><input type={state.question!.multiSelect ? "checkbox" : "radio"} name="agent-question" checked={questionSelections.includes(option)} onChange={() => toggleQuestionOption(option)} /> {option}</label>)}</div><input aria-label="Other answer" value={questionText} onChange={(event) => setQuestionText(event.target.value)} placeholder={state.question.options.length > 0 ? "Other or additional context" : "Your answer"} /></div>
          <div className="decision-actions"><button className="primary-button" disabled={questionSelections.length === 0 && !questionText.trim()}>Answer</button></div>
        </form>}
        {state.approval && state.approval.tools[0] && <section className="decision-dock" aria-labelledby="approval-title">
          <div className="decision-icon" aria-hidden="true">◇</div>
          <div className="decision-copy"><span className="eyebrow">Approval · {state.approval.tools.length} pending</span><h3 id="approval-title">Allow {state.approval.tools[0].toolName}?</h3><p>Runtime is waiting for the least-authorizing decision.</p></div>
          <div className="decision-actions"><button className="secondary-button" onClick={() => void decide(state.approval!.tools[0].callId, false)}>Reject</button>{state.approval.allowedScopes.map((scope) => <button key={scope} className="primary-button" onClick={() => void decide(state.approval!.tools[0].callId, true, scope)}>{approvalScopeLabel(scope)}</button>)}</div>
        </section>}
        {state.feedback && state.protocol?.capabilities.includes("feedback_v1") && <section className="decision-dock" aria-labelledby="feedback-title">
          <div className="decision-icon" aria-hidden="true">◇</div>
          <div className="decision-copy"><span className="eyebrow">Private turn feedback</span><h3 id="feedback-title">Was this response useful?</h3>{state.feedback.status === "recorded" ? <p role="status">Feedback recorded.</p> : <label>Optional note<input aria-label="Feedback note" maxLength={4096} value={feedbackNote} onChange={(event) => setFeedbackNote(event.target.value)} disabled={state.feedback.status === "submitting"} /></label>}</div>
          {state.feedback.status !== "recorded" && <div className="decision-actions"><button type="button" className="secondary-button" disabled={state.feedback.status === "submitting"} onClick={() => void recordFeedback("negative")}>Needs improvement</button><button type="button" className="primary-button" disabled={state.feedback.status === "submitting"} onClick={() => void recordFeedback("positive")}>Useful</button></div>}
        </section>}
        <form className="composer" onSubmit={(event) => void send(event)}>
          <label htmlFor="composer-input" className="sr-only">Message Sylvander</label>
          <textarea id="composer-input" value={draft} onChange={(event) => updateDraft(event.target.value)} rows={2} placeholder="What should we work through?" onKeyDown={handleComposerKey} disabled={!selected || state.connection !== "live" || selected.state === "active" || selected.state === "waiting"} />
          <div className="composer-footer">
            <div className="composer-tools"><button type="button" aria-label="Attach context">＋</button><button type="button" onClick={openRuntimeSettings}>{reasoningLabel(state.runtimeInfo?.reasoningEffort)} <span>⌄</span></button><button type="button" onClick={openRuntimeSettings}>{state.runtimeInfo ? `${state.runtimeInfo.providerId}/${state.runtimeInfo.modelId}` : "Runtime model"} <span>⌄</span></button></div>
            <div className="send-group"><span><kbd>↵</kbd> send · <kbd>⇧↵</kbd> line</span>{selected && ["active", "waiting"].includes(selected.state)
              ? <button type="button" className="send-button" disabled={state.connection !== "live" || state.interruptingSessionIds.includes(selected.id)} aria-label="Stop" onClick={() => void interruptTurn(selected.id)}>{state.interruptingSessionIds.includes(selected.id) ? "…" : "■"}</button>
              : <button className="send-button" disabled={!draft.trim() || !selected || state.connection !== "live"} aria-label="Send">↑</button>}</div>
          </div>
        </form>
      </div>
    </main>

    {inspectorOpen && <aside className="inspector" aria-label="Session inspector">
      <header><div><span className="eyebrow">Live work</span><h2>Execution</h2></div><button className="icon-button" onClick={() => setInspectorOpen(false)} aria-label="Close inspector">×</button></header>
      <div className="inspector-tabs" role="tablist" aria-label="Execution details">
        {(["plan", "tasks", "context", "changes"] as const).map((tab) => <button key={tab} role="tab" aria-selected={inspector === tab} className={inspector === tab ? "active" : ""} onClick={() => setInspector(tab)}>{tab}</button>)}
      </div>
      {inspector === "plan" && <ol className="plan-list">{state.plan.map((step, index) => <li key={`${index}-${step.label}`} data-state={step.state}><span>{step.state === "complete" ? "✓" : index + 1}</span><p>{step.label}</p></li>)}</ol>}
      {inspector === "plan" && state.activePlan && <form className="plan-editor" onSubmit={(event) => void revisePlan(event)}><fieldset><legend>Revise plan</legend>{planRevision.map((step, index) => <label key={index}>Step {index + 1}<input value={step} onChange={(event) => updatePlanStep(index, event.target.value)} /></label>)}</fieldset><div className="decision-actions"><button type="button" className="secondary-button" onClick={() => void resolvePlan(state.activePlan!.planId, { decision: "rejected", reason: "cancelled by user" })}>Reject plan</button><button type="submit" className="secondary-button" disabled={planRevision.every((step) => !step.trim())}>Submit revision</button><button type="button" className="primary-button" onClick={() => void resolvePlan(state.activePlan!.planId, { decision: "approved" })}>Approve plan</button></div></form>}
      {inspector === "tasks" && <div className="task-list">{state.tasks.map((task) => <article key={task.id}><span className={`presence ${task.state}`} /><div><strong>{task.purpose}</strong><p>{task.owner} · {task.state}{task.detail ? ` · ${task.detail}` : ""}</p></div>{task.state === "running" && <button className="secondary-button" onClick={() => void cancelTask(task.id)}>Cancel</button>}</article>)}</div>}
      {inspector === "context" && <section className="context-panel">
        {state.contextReport ? <><h3>{state.contextReport.model}</h3><p>{state.contextReport.used_tokens} / {state.contextReport.context_window} tokens · {contextPercent(state.contextReport.used_tokens, state.contextReport.context_window)}%</p><p>{state.contextReport.remaining_tokens} remaining · cache {state.contextReport.cache_read_tokens} read / {state.contextReport.cache_write_tokens} written</p><ul>{state.contextReport.sources.map((source) => <li key={`${source.kind}-${source.label}`}>{source.label} · {source.items}</li>)}</ul></> : <p>Request Runtime's provider-confirmed context report.</p>}
        {state.compaction?.status === "running" && <p role="status">{state.compaction.automatic ? "Automatic compaction" : "Compaction"} in progress…</p>}
        {state.compaction?.status === "completed" && state.compaction.report && <p>{state.compaction.report.removed_messages} messages removed · {state.compaction.report.condensed_blocks} blocks condensed · ~{state.compaction.report.freed_tokens} tokens freed{state.compaction.report.summary ? ` · ${state.compaction.report.summary}` : ""}</p>}
        {state.compaction?.status === "failed" && <p role="alert">Compaction failed · {state.compaction.reason}</p>}
        <div className="context-actions"><button className="secondary-button" disabled={!selected || state.connection !== "live" || state.contextRequestPending} onClick={() => selected && void requestContext(selected.id)}>Refresh</button><button className="primary-button" disabled={!selected || state.connection !== "live" || state.compaction?.status === "running"} onClick={() => selected && void compactContext(selected.id)}>Compact</button></div>
      </section>}
      {inspector === "changes" && <section className="changes-panel">
        {!state.codingReview && <div className="empty-inspector"><span>±</span><h3>Review coding Session</h3><p>Load tracked and untracked changes from Runtime's isolated worktree.</p></div>}
        {state.codingReview && (state.codingReview.status || state.codingReview.patch) && <pre>{[state.codingReview.status && `git status --short\n${state.codingReview.status}`, state.codingReview.patch && `git diff HEAD\n${state.codingReview.patch}`].filter(Boolean).join("\n\n")}</pre>}
        {state.codingReview?.outcome === "accepted" && <p role="status">Reviewed changes merged by Runtime.</p>}
        {state.codingReview?.outcome === "failed" && <p role="alert">Coding Session operation failed · {state.codingReview.detail}</p>}
        <div className="context-actions"><button className="secondary-button" disabled={!selected || state.connection !== "live"} onClick={() => selected && void submit({ type: "inspect_coding_session", session_id: selected.id })}>Load changes</button>{state.codingReview && (state.codingReview.status || state.codingReview.patch) && <><button className="primary-button" onClick={() => selected && void submit({ type: "accept_coding_session", session_id: selected.id })}>Accept</button><button className="secondary-button" onClick={() => selected && void submit({ type: "discard_coding_session", session_id: selected.id })}>Discard Session</button></>}</div>
        <hr />
        <h3>Workspace rollback</h3>
        {state.rollback?.status === "preview" && <><p>Runtime can restore {state.rollback.files.length} files from turn {state.rollback.turnId?.slice(0, 8)}. Conversation history stays unchanged.</p><ul>{state.rollback.files.map((file) => <li key={file}>{file}</li>)}</ul></>}
        {state.rollback?.status === "completed" && <p role="status">{state.rollback.files.length} files restored · conversation history unchanged.</p>}
        {state.rollback?.status === "failed" && <p role="alert">Workspace rollback failed · {state.rollback.detail}</p>}
        <div className="context-actions"><button className="secondary-button" disabled={!selected || state.connection !== "live"} onClick={() => selected && void submit({ type: "preview_workspace_rollback", session_id: selected.id })}>Preview rollback</button>{state.rollback?.status === "preview" && state.rollback.turnId && <button className="primary-button" onClick={() => selected && void submit({ type: "rollback_workspace", session_id: selected.id, expected_turn_id: state.rollback!.turnId! })}>Confirm rollback</button>}</div>
      </section>}
      <footer className="inspector-summary"><span>{state.sessionStats ? `${state.sessionStats.iterations} iterations · ${state.sessionStats.inputTokens + state.sessionStats.outputTokens} tokens${state.sessionStats.costNanoUsd === undefined ? "" : ` · ${formatCost(state.sessionStats.costNanoUsd)}`}${state.sessionStats.sourceSessionId ? ` · fork of ${state.sessionStats.sourceSessionId.slice(0, 8)}` : ""}` : "Protocol"}</span><strong>{state.protocol ? `v${state.protocol.version}` : "—"}</strong><div><span style={{ width: state.connection === "live" ? "100%" : "0%" }} /></div></footer>
    </aside>}
    <div className="sr-only" aria-live="polite">{connectionLabel(state.connection)}</div>
  </div>;
}

function connectionLabel(state: string) {
  return state === "live" ? "Connected" : state.charAt(0).toUpperCase() + state.slice(1);
}

function reasoningLabel(effort?: string) {
  if (!effort || effort === "off") return "Standard";
  return `${effort.charAt(0).toUpperCase()}${effort.slice(1)} reasoning`;
}

function permissionLabel(info: NonNullable<RuntimeViewState["runtimeInfo"]>) {
  const files = info.fileAccess === "workspace_write"
    ? "workspace write"
    : info.fileAccess === "read_only" ? "read only" : "no files";
  return `${files} · network ${info.networkAccess} · approval ${info.approvalPolicy}`;
}

function approvalScopeLabel(scope: ApprovalScope) {
  switch (scope) {
    case "once": return "Allow once";
    case "session": return "Allow for Session";
    case "persistent": return "Always allow";
  }
}

function formatCost(costNanoUsd: number) {
  return `$${(costNanoUsd / 1_000_000_000).toFixed(6)}`;
}

function contextPercent(usedTokens: number, contextWindow: number) {
  return contextWindow === 0 ? 0 : Math.floor((usedTokens * 100) / contextWindow);
}
