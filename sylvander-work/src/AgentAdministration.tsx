import { useState } from "react";

import type { RuntimeAgentAdminRequest } from "./lib/gateway";
import type { RuntimeViewState } from "./lib/useRuntime";

interface AgentAdministrationProps {
  agents: RuntimeViewState["agents"];
  state: RuntimeViewState["agentAdministration"];
  onClose(): void;
  onRequest(request: RuntimeAgentAdminRequest): Promise<boolean>;
}

export function AgentAdministration({ agents, state, onClose, onRequest }: AgentAdministrationProps) {
  const [agentId, setAgentId] = useState(state.agentId ?? agents[0]?.id ?? "");
  const [armedRevision, setArmedRevision] = useState<number>();
  const busy = state.status === "loading" || state.status === "submitting";

  function load(selectedAgentId = agentId, beforeRevision?: number) {
    if (!selectedAgentId) return;
    setAgentId(selectedAgentId);
    setArmedRevision(undefined);
    void onRequest({
      operation: "list_revisions",
      agent_id: selectedAgentId,
      ...(beforeRevision === undefined ? {} : { before_revision: beforeRevision }),
      limit: 50,
    });
  }

  function changeActive(revision: number) {
    if (!state.agentId || state.activeRevision === undefined) return;
    const request: RuntimeAgentAdminRequest = revision < state.activeRevision
      ? {
          operation: "rollback_revision",
          agent_id: state.agentId,
          target_revision: revision,
          expected_active_revision: state.activeRevision,
        }
      : {
          operation: "activate_revision",
          agent_id: state.agentId,
          revision,
          expected_active_revision: state.activeRevision,
        };
    void onRequest(request);
  }

  return <aside className="inspector account-panel" aria-label="Agent administration">
    <header><div><span className="eyebrow">Privileged control plane</span><h2>Agent Revisions</h2></div><button className="icon-button" type="button" aria-label="Close Agent administration" onClick={onClose}>×</button></header>
    <section className="context-panel">
      <p>Runtime authorizes every operation. This surface renders only redacted views and digests; it cannot recover prompts, paths, principal names, or MCP commands.</p>
      <label>Agent<select aria-label="Administrative Agent" value={agentId} onChange={(event) => load(event.target.value)}><option value="">Select an Agent</option>{agents.map((agent) => <option key={agent.id} value={agent.id}>{agent.name}</option>)}</select></label>
      <button type="button" className="secondary-button" disabled={!agentId || busy} onClick={() => load()}>Load revisions</button>
      {state.notice && <p role={state.status === "error" ? "alert" : "status"}>{state.notice}</p>}
      {state.status === "loading" && <p role="status">Loading redacted revisions…</p>}
      {state.revisions.map((revision) => {
        const definition = revision.definition;
        const changing = armedRevision === definition.revision;
        return <article key={definition.revision} className="admin-revision">
          <h3>Revision {definition.revision}{revision.active ? " · active" : ""}</h3>
          <p>{definition.provider_id}/{definition.default_model_id} · {definition.tools.length} tools · {definition.hooks.length} hooks · {definition.workspace_mount_count} mounts</p>
          <p>definition {revision.digest_sha256} · prompt {definition.system_prompt_sha256}</p>
          <p>Access: authenticated {definition.access.allow_authenticated ? "allowed" : "denied"} · {definition.access.allowed_principal_count} explicit principals · {definition.access.allowed_roles.length} roles</p>
          {!revision.active && (changing ? <button type="button" className="primary-button" disabled={busy} onClick={() => changeActive(definition.revision)}>Confirm {state.activeRevision !== undefined && definition.revision < state.activeRevision ? "rollback" : "activation"} from revision {state.activeRevision}</button> : <button type="button" className="secondary-button" disabled={busy || state.activeRevision === undefined} onClick={() => setArmedRevision(definition.revision)}>Make active…</button>)}
        </article>;
      })}
      {state.nextBeforeRevision !== undefined && <button type="button" className="secondary-button" disabled={busy} onClick={() => load(state.agentId, state.nextBeforeRevision)}>Load older revisions</button>}
    </section>
  </aside>;
}
