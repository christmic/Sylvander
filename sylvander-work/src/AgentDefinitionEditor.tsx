import { FormEvent, useState } from "react";

import { CommandRows, HookRows, McpRows, ModelRows, MountRows, PairRows, PresentationRows, PromptProfileRows, StringRows, WorkspaceBinding } from "./AgentDefinitionFields";
import type { RuntimeAgentDefinitionDraft, RuntimeAgentSecretReference, RuntimeAgentToolDraft, RuntimeModelSelection, RuntimeWorkspaceMount } from "./lib/gateway";

interface AgentDefinitionEditorProps {
  agentId: string;
  activeRevision: number;
  onCancel(): void;
  onSubmit(definition: RuntimeAgentDefinitionDraft): Promise<boolean>;
}

export interface McpToolEditor {
  name: string;
  executionEnvironment: string;
  workspaceAccess: "read" | "write";
  command: string;
  args: string[];
  environment: Array<{ variable: string; source: "environment" | "file"; reference: string }>;
}

export function AgentDefinitionEditor({ agentId, activeRevision, onCancel, onSubmit }: AgentDefinitionEditorProps) {
  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [providerId, setProviderId] = useState("");
  const [modelId, setModelId] = useState("");
  const [allowedModels, setAllowedModels] = useState<RuntimeModelSelection[]>([]);
  const [temperature, setTemperature] = useState("");
  const [maxTokens, setMaxTokens] = useState("");
  const [systemPrompt, setSystemPrompt] = useState("");
  const [builtins, setBuiltins] = useState<string[]>([]);
  const [mcpTools, setMcpTools] = useState<McpToolEditor[]>([]);
  const [memoryStores, setMemoryStores] = useState<Array<{ store_type: string; path: string }>>([]);
  const [commands, setCommands] = useState<Array<{ id: string; name: string; usage: string; description: string; hint: string; prompt: string }>>([]);
  const [hooks, setHooks] = useState<RuntimeAgentDefinitionDraft["hooks"]>([]);
  const [presentations, setPresentations] = useState<RuntimeAgentDefinitionDraft["tool_presentations"]>([]);
  const [maxIterations, setMaxIterations] = useState("50");
  const [maxRetries, setMaxRetries] = useState("3");
  const [agentWorkspace, setAgentWorkspace] = useState<RuntimeAgentDefinitionDraft["agent_workspace"]>();
  const [mounts, setMounts] = useState<RuntimeWorkspaceMount[]>([]);
  const [promptProfiles, setPromptProfiles] = useState<RuntimeAgentDefinitionDraft["prompt_profiles"]>([]);
  const [defaultProfile, setDefaultProfile] = useState("");
  const [allowSessionPrompt, setAllowSessionPrompt] = useState(false);
  const [allowAuthenticated, setAllowAuthenticated] = useState(false);
  const [principals, setPrincipals] = useState<string[]>([]);
  const [roles, setRoles] = useState<string[]>([]);
  const [validation, setValidation] = useState("");

  function qualifiedModels() {
    const defaultModel = { provider_id: providerId.trim(), model_id: modelId.trim() };
    return uniqueModels([defaultModel, ...allowedModels]);
  }

  async function submit(event: FormEvent) {
    event.preventDefault();
    const required = [name, providerId, modelId, systemPrompt].every((value) => value.trim());
    if (!required || Number(maxIterations) < 1) {
      setValidation("Name, provider, default model, prompt, and positive iteration limit are required");
      return;
    }
    const tools: RuntimeAgentToolDraft[] = [
      ...builtins.filter((item) => item.trim()).map((item) => ({ type: "builtin" as const, name: item.trim() })),
      ...mcpTools.map((tool) => ({
        type: "mcp_server" as const,
        name: tool.name.trim(),
        execution_environment: tool.executionEnvironment.trim(),
        workspace_access: tool.workspaceAccess,
        command: tool.command.trim(),
        args: clean(tool.args),
        environment: Object.fromEntries(tool.environment.filter((entry) => entry.variable.trim() && entry.reference.trim()).map((entry) => [
          entry.variable.trim(),
          entry.source === "environment"
            ? { source: "environment", name: entry.reference.trim() }
            : { source: "file", path: entry.reference.trim() },
        ])) as Record<string, RuntimeAgentSecretReference>,
      })),
    ];
    const definition: RuntimeAgentDefinitionDraft = {
      agent_id: agentId,
      revision: activeRevision + 1,
      name: name.trim(),
      description: description.trim(),
      provider_id: providerId.trim(),
      default_model_id: modelId.trim(),
      allowed_models: qualifiedModels(),
      ...(temperature ? { temperature: Number(temperature) } : {}),
      ...(maxTokens ? { max_tokens: Number(maxTokens) } : {}),
      system_prompt: systemPrompt,
      tools,
      memory_stores: memoryStores.filter((item) => item.store_type.trim() && item.path.trim()),
      ui_commands: commands.filter((item) => item.id.trim() && item.name.trim() && item.prompt.trim()),
      hooks: hooks.filter((item) => item.name.trim() && item.command.trim()),
      tool_presentations: presentations.filter((item) => item.tool_name.trim() && item.label.trim()),
      behavior: { max_iterations: Number(maxIterations), max_retries: Number(maxRetries) },
      ...(agentWorkspace?.execution_target.trim() && agentWorkspace.path.trim() ? { agent_workspace: agentWorkspace } : {}),
      workspace_mounts: mounts.filter((item) => item.reference.trim() && item.binding.execution_target.trim() && item.binding.path.trim()),
      prompt_profiles: promptProfiles.filter((item) => item.id.trim() && item.system_prompt.trim()),
      ...(defaultProfile ? { default_prompt_profile: defaultProfile } : {}),
      allow_session_prompt: allowSessionPrompt,
      access: { allow_authenticated: allowAuthenticated, allowed_principals: clean(principals), allowed_roles: clean(roles) },
    };
    if (await onSubmit(definition)) onCancel();
  }

  return <form className="definition-editor" onSubmit={submit} aria-label="Agent definition editor">
    <header><div><span className="eyebrow">Write-only candidate</span><h3>Stage revision {activeRevision + 1}</h3></div><button type="button" className="icon-button" aria-label="Close definition editor" onClick={onCancel}>×</button></header>
    <p>The current redacted revision cannot populate secret-bearing fields. Supply a complete replacement; staging never activates it.</p>
    {validation && <p role="alert">{validation}</p>}
    <details open><summary>Identity and model</summary><div className="definition-grid"><Text name="Agent name" value={name} set={setName} /><Text name="Description" value={description} set={setDescription} /><Text name="Provider id" value={providerId} set={setProviderId} /><Text name="Default model id" value={modelId} set={setModelId} /><NumberField name="Temperature" value={temperature} set={setTemperature} step="0.1" /><NumberField name="Max output tokens" value={maxTokens} set={setMaxTokens} /><label className="wide">System prompt<textarea aria-label="System prompt" value={systemPrompt} onChange={(event) => setSystemPrompt(event.target.value)} /></label></div><ModelRows label="Additional allowed models" values={allowedModels} set={setAllowedModels} /></details>
    <details><summary>Tools and memory</summary><StringRows label="Built-in tools" values={builtins} set={setBuiltins} /><McpRows values={mcpTools} set={setMcpTools} /><PairRows label="Memory stores" left="Store type" right="Path" values={memoryStores.map((item) => [item.store_type, item.path])} set={(rows) => setMemoryStores(rows.map(([store_type, path]) => ({ store_type, path })))} /></details>
    <details><summary>UI commands and hooks</summary><CommandRows values={commands} set={setCommands} /><HookRows values={hooks} set={setHooks} /><PresentationRows values={presentations} set={setPresentations} /></details>
    <details><summary>Execution environment</summary><div className="definition-grid"><NumberField name="Max iterations" value={maxIterations} set={setMaxIterations} /><NumberField name="Max retries" value={maxRetries} set={setMaxRetries} /></div><WorkspaceBinding value={agentWorkspace} set={setAgentWorkspace} /><MountRows values={mounts} set={setMounts} /></details>
    <details><summary>Prompt profiles and access</summary><PromptProfileRows values={promptProfiles} set={setPromptProfiles} /><label>Default prompt profile<select aria-label="Default prompt profile" value={defaultProfile} onChange={(event) => setDefaultProfile(event.target.value)}><option value="">none</option>{promptProfiles.map((profile) => <option key={profile.id} value={profile.id}>{profile.id || "unnamed"}</option>)}</select></label><label><input type="checkbox" checked={allowSessionPrompt} onChange={(event) => setAllowSessionPrompt(event.target.checked)} /> Allow Session prompt override</label><label><input type="checkbox" checked={allowAuthenticated} onChange={(event) => setAllowAuthenticated(event.target.checked)} /> Allow authenticated principals</label><StringRows label="Allowed principals" values={principals} set={setPrincipals} /><StringRows label="Allowed roles" values={roles} set={setRoles} /></details>
    <div className="context-actions"><button type="button" className="secondary-button" onClick={onCancel}>Cancel</button><button className="primary-button">Stage complete definition</button></div>
  </form>;
}

function Text({ name, value, set }: { name: string; value: string; set(value: string): void }) { return <label>{name}<input aria-label={name} value={value} onChange={(event) => set(event.target.value)} /></label>; }
function NumberField({ name, value, set, step }: { name: string; value: string; set(value: string): void; step?: string }) { return <label>{name}<input aria-label={name} type="number" min="0" step={step ?? "1"} value={value} onChange={(event) => set(event.target.value)} /></label>; }
function clean(values: string[]) { return values.map((value) => value.trim()).filter(Boolean); }
function uniqueModels(values: RuntimeModelSelection[]) { return values.filter((value, index, all) => value.provider_id && value.model_id && all.findIndex((candidate) => candidate.provider_id === value.provider_id && candidate.model_id === value.model_id) === index); }
