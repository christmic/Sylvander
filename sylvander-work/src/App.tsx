import { FormEvent, KeyboardEvent, useEffect, useMemo, useState } from "react";

import crabMark from "../../docs/design/final-brand/sylvander-seed-crab-character-square.png";
import type { RuntimeGatewayPort } from "./lib/gateway";
import { useRuntime } from "./lib/useRuntime";

export interface AppProps {
  gateway?: RuntimeGatewayPort;
}

export default function App({ gateway }: AppProps) {
  const { state, selectSession, submit, answerQuestion, resolvePlan, cancelTask } = useRuntime(gateway);
  const [query, setQuery] = useState("");
  const [drafts, setDrafts] = useState<Record<string, string>>({});
  const [inspector, setInspector] = useState<"plan" | "tasks" | "changes">("plan");
  const [inspectorOpen, setInspectorOpen] = useState(false);
  const [sidebarOpen, setSidebarOpen] = useState(false);
  const [questionSelections, setQuestionSelections] = useState<string[]>([]);
  const [questionText, setQuestionText] = useState("");
  const [planRevision, setPlanRevision] = useState<string[]>([]);
  const [createOpen, setCreateOpen] = useState(false);
  const [newSessionLabel, setNewSessionLabel] = useState("New session");
  const [newSessionAgentId, setNewSessionAgentId] = useState("");
  const [sessionActionsOpen, setSessionActionsOpen] = useState(false);
  const [sessionLabel, setSessionLabel] = useState("");
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

  function updateDraft(value: string) {
    if (state.selectedId) setDrafts((current) => ({ ...current, [state.selectedId!]: value }));
  }

  async function send(event?: FormEvent) {
    event?.preventDefault();
    if (!state.selectedId || state.connection !== "live" || !draft.trim()) return;
    await submit({ type: "chat", text: draft.trim(), attachments: [], session_id: state.selectedId });
    updateDraft("");
  }

  function handleComposerKey(event: KeyboardEvent<HTMLTextAreaElement>) {
    if (event.key === "Enter" && !event.shiftKey) {
      event.preventDefault();
      void send();
    }
  }

  async function decide(callId: string, approved: boolean) {
    if (!state.approval) return;
    await submit({
      type: "approve",
      session_id: state.approval.sessionId,
      call_id: callId,
      approved,
      scope: "once",
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

  async function deleteSession() {
    if (!selected) return;
    await submit({ type: "delete_session", session_id: selected.id });
    setSessionActionsOpen(false);
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
      <div className="session-section-label"><span>Recent</span><span>{visibleSessions.length}</span></div>
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
      <footer className="runtime-card">
        <span className={`runtime-dot ${state.connection}`} />
        <div><strong>Local Runtime</strong><span>{connectionLabel(state.connection)}</span></div>
        <button aria-label="Runtime details">···</button>
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
        {sessionActionsOpen && selected && <form className="decision-dock" aria-labelledby="session-actions-title" onSubmit={(event) => void renameSession(event)}><div className="decision-icon" aria-hidden="true">···</div><div className="decision-copy"><span className="eyebrow">Runtime Session</span><h3 id="session-actions-title">Manage {selected.label}</h3><label>Name<input aria-label="Session label" value={sessionLabel} onChange={(event) => setSessionLabel(event.target.value)} /></label><p>Archive hides the Session from active work. Delete permanently removes it through Runtime policy.</p></div><div className="decision-actions"><button type="button" className="secondary-button" onClick={() => void archiveSession()}>Archive</button><button type="button" className="secondary-button" onClick={() => void deleteSession()}>Delete permanently</button><button className="primary-button" disabled={!sessionLabel.trim()}>Rename</button></div></form>}
        {createOpen && <form className="decision-dock" aria-labelledby="create-session-title" onSubmit={(event) => void createSession(event)}><div className="decision-icon" aria-hidden="true">＋</div><div className="decision-copy"><span className="eyebrow">Runtime Session</span><h3 id="create-session-title">Create Session</h3><label>Name<input aria-label="Session name" value={newSessionLabel} onChange={(event) => setNewSessionLabel(event.target.value)} /></label><label>Agent<select aria-label="Session Agent" value={newSessionAgentId} onChange={(event) => setNewSessionAgentId(event.target.value)}>{state.agents.map((agent) => <option key={agent.id} value={agent.id}>{agent.name} · {agent.providerId}/{agent.modelId}</option>)}</select></label></div><div className="decision-actions"><button type="button" className="secondary-button" onClick={() => setCreateOpen(false)}>Cancel</button><button className="primary-button" disabled={!newSessionLabel.trim() || !newSessionAgentId}>Create</button></div></form>}
        {state.question && <form className="decision-dock" aria-labelledby="question-title" onSubmit={(event) => void submitQuestion(event)}>
          <div className="decision-icon" aria-hidden="true">?</div>
          <div className="decision-copy"><span className="eyebrow">Agent asks</span><h3 id="question-title">{state.question.prompt}</h3><div className="question-options">{state.question.options.map((option) => <label key={option}><input type={state.question!.multiSelect ? "checkbox" : "radio"} name="agent-question" checked={questionSelections.includes(option)} onChange={() => toggleQuestionOption(option)} /> {option}</label>)}</div><input aria-label="Other answer" value={questionText} onChange={(event) => setQuestionText(event.target.value)} placeholder={state.question.options.length > 0 ? "Other or additional context" : "Your answer"} /></div>
          <div className="decision-actions"><button className="primary-button" disabled={questionSelections.length === 0 && !questionText.trim()}>Answer</button></div>
        </form>}
        {state.approval && state.approval.tools[0] && <section className="decision-dock" aria-labelledby="approval-title">
          <div className="decision-icon" aria-hidden="true">◇</div>
          <div className="decision-copy"><span className="eyebrow">Approval · {state.approval.tools.length} pending</span><h3 id="approval-title">Allow {state.approval.tools[0].toolName}?</h3><p>Runtime is waiting for the least-authorizing decision.</p></div>
          <div className="decision-actions"><button className="secondary-button" onClick={() => void decide(state.approval!.tools[0].callId, false)}>Reject</button><button className="primary-button" onClick={() => void decide(state.approval!.tools[0].callId, true)}>Allow once</button></div>
        </section>}
        <form className="composer" onSubmit={(event) => void send(event)}>
          <label htmlFor="composer-input" className="sr-only">Message Sylvander</label>
          <textarea id="composer-input" value={draft} onChange={(event) => updateDraft(event.target.value)} rows={2} placeholder="What should we work through?" onKeyDown={handleComposerKey} disabled={!selected || state.connection !== "live"} />
          <div className="composer-footer">
            <div className="composer-tools"><button type="button" aria-label="Attach context">＋</button><button type="button">Standard <span>⌄</span></button><button type="button">Runtime model <span>⌄</span></button></div>
            <div className="send-group"><span><kbd>↵</kbd> send · <kbd>⇧↵</kbd> line</span><button className="send-button" disabled={!draft.trim() || !selected || state.connection !== "live"} aria-label="Send">↑</button></div>
          </div>
        </form>
      </div>
    </main>

    {inspectorOpen && <aside className="inspector" aria-label="Session inspector">
      <header><div><span className="eyebrow">Live work</span><h2>Execution</h2></div><button className="icon-button" onClick={() => setInspectorOpen(false)} aria-label="Close inspector">×</button></header>
      <div className="inspector-tabs" role="tablist" aria-label="Execution details">
        {(["plan", "tasks", "changes"] as const).map((tab) => <button key={tab} role="tab" aria-selected={inspector === tab} className={inspector === tab ? "active" : ""} onClick={() => setInspector(tab)}>{tab}</button>)}
      </div>
      {inspector === "plan" && <ol className="plan-list">{state.plan.map((step, index) => <li key={`${index}-${step.label}`} data-state={step.state}><span>{step.state === "complete" ? "✓" : index + 1}</span><p>{step.label}</p></li>)}</ol>}
      {inspector === "plan" && state.activePlan && <form className="plan-editor" onSubmit={(event) => void revisePlan(event)}><fieldset><legend>Revise plan</legend>{planRevision.map((step, index) => <label key={index}>Step {index + 1}<input value={step} onChange={(event) => updatePlanStep(index, event.target.value)} /></label>)}</fieldset><div className="decision-actions"><button type="button" className="secondary-button" onClick={() => void resolvePlan(state.activePlan!.planId, { decision: "rejected", reason: "cancelled by user" })}>Reject plan</button><button type="submit" className="secondary-button" disabled={planRevision.every((step) => !step.trim())}>Submit revision</button><button type="button" className="primary-button" onClick={() => void resolvePlan(state.activePlan!.planId, { decision: "approved" })}>Approve plan</button></div></form>}
      {inspector === "tasks" && <div className="task-list">{state.tasks.map((task) => <article key={task.id}><span className={`presence ${task.state}`} /><div><strong>{task.purpose}</strong><p>{task.owner} · {task.state}{task.detail ? ` · ${task.detail}` : ""}</p></div>{task.state === "running" && <button className="secondary-button" onClick={() => void cancelTask(task.id)}>Cancel</button>}</article>)}</div>}
      {inspector === "changes" && <div className="empty-inspector"><span>±</span><h3>No reviewable diff</h3><p>Runtime-owned changes will appear here.</p></div>}
      <footer className="inspector-summary"><span>Protocol</span><strong>v5</strong><div><span style={{ width: state.connection === "live" ? "100%" : "0%" }} /></div></footer>
    </aside>}
    <div className="sr-only" aria-live="polite">{connectionLabel(state.connection)}</div>
  </div>;
}

function connectionLabel(state: string) {
  return state === "live" ? "Connected" : state.charAt(0).toUpperCase() + state.slice(1);
}
