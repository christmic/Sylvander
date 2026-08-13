import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { AgentDefinitionEditor } from "./AgentDefinitionEditor";
import type { RuntimeAgentDefinitionDraft } from "./lib/gateway";

describe("AgentDefinitionEditor", () => {
  it("builds the complete write-only public draft without inspection backfill", async () => {
    const submit = vi.fn(async (_definition: RuntimeAgentDefinitionDraft) => true);
    render(<AgentDefinitionEditor
      agentId="agent-1"
      activeRevision={4}
      onCancel={() => undefined}
      onSubmit={submit}
    />);
    change("Agent name", "Coding Agent");
    change("Description", "Works on code");
    change("Provider id", "openai");
    change("Default model id", "gpt-test");
    change("Temperature", "0.2");
    change("Max output tokens", "4096");
    change("System prompt", "private write-only prompt");
    click("Add Additional allowed models");
    change("Additional allowed models provider 1", "anthropic");
    change("Additional allowed models model 1", "claude-test");

    open("Tools and memory");
    click("Add Built-in tools");
    change("Built-in tools 1", "Read");
    click("Add MCP server");
    open("MCP 1 · unnamed");
    change("MCP name 1", "search");
    change("MCP execution environment 1", "sandbox");
    change("MCP workspace access 1", "write");
    change("MCP command 1", "mcp-search");
    click("Add MCP arguments 1");
    change("MCP arguments 1 1", "serve");
    click("Add MCP secret reference 1");
    change("MCP variable 1.1", "TOKEN");
    change("MCP secret source 1.1", "file");
    change("MCP secret reference 1.1", "/run/secrets/token");
    click("Add Memory stores");
    change("Store type 1", "sqlite");
    change("Path 1", "/data/memory.db");

    open("UI commands and hooks");
    click("Add UI command");
    open("Command 1 · unnamed");
    for (const [field, value] of Object.entries({ id: "review", name: "Review", usage: "/review", description: "Review work", hint: "optional", prompt: "private command prompt" })) {
      change(`Command ${field} 1`, value);
    }
    click("Add hook");
    change("Hook name 1", "verify");
    change("Hook phase 1", "after_turn");
    change("Hook command 1", "verify-result");
    change("Hook timeout 1", "20");
    fireEvent.click(screen.getByRole("checkbox", { name: /blocking/ }));
    click("Add tool presentation");
    change("Presentation tool 1", "Read");
    change("Presentation label 1", "Read file");
    change("Presentation kind 1", "file");
    change("Presentation target 1", "path");

    open("Execution environment");
    change("Max iterations", "60");
    change("Max retries", "4");
    click("Configure Agent workspace");
    change("Agent workspace execution target", "sandbox");
    change("Agent workspace path", "/workspace/agent");
    change("Agent workspace instruction focus", "src");
    click("Add workspace mount");
    open("Mount 1 · unnamed");
    change("Mount reference 1", "dependency");
    change("Mount role 1", "dependency");
    change("Mount execution target 1", "sandbox");
    change("Mount path 1", "/workspace/dependency");
    fireEvent.click(screen.getByRole("checkbox", { name: "Mount command 1" }));

    open("Prompt profiles and access");
    click("Add prompt profile");
    open("Profile 1 · unnamed");
    change("Prompt profile id 1", "claude");
    change("Prompt profile system prompt 1", "private model prompt");
    click("Add Prompt profile models 1");
    change("Prompt profile models 1 provider 1", "anthropic");
    change("Prompt profile models 1 model 1", "claude-test");
    change("Default prompt profile", "claude");
    fireEvent.click(screen.getByRole("checkbox", { name: /Allow Session prompt override/ }));
    fireEvent.click(screen.getByRole("checkbox", { name: /Allow authenticated principals/ }));
    click("Add Allowed principals");
    change("Allowed principals 1", "operator-1");
    click("Add Allowed roles");
    change("Allowed roles 1", "operator");

    click("Stage complete definition");
    expect(submit).toHaveBeenCalledOnce();
    expect(submit.mock.calls[0][0]).toMatchObject({
      agent_id: "agent-1",
      revision: 5,
      provider_id: "openai",
      default_model_id: "gpt-test",
      allowed_models: [
        { provider_id: "openai", model_id: "gpt-test" },
        { provider_id: "anthropic", model_id: "claude-test" },
      ],
      temperature: 0.2,
      max_tokens: 4096,
      system_prompt: "private write-only prompt",
      tools: [
        { type: "builtin", name: "Read" },
        {
          type: "mcp_server",
          execution_environment: "sandbox",
          workspace_access: "write",
          args: ["serve"],
          environment: { TOKEN: { source: "file", path: "/run/secrets/token" } },
        },
      ],
      memory_stores: [{ store_type: "sqlite", path: "/data/memory.db" }],
      behavior: { max_iterations: 60, max_retries: 4 },
      agent_workspace: { execution_target: "sandbox", path: "/workspace/agent", read_only: false, instruction_focus: "src" },
      workspace_mounts: [{ reference: "dependency", capabilities: { read: true, write: false, command: true, git: false } }],
      prompt_profiles: [{ id: "claude", qualified_models: [{ provider_id: "anthropic", model_id: "claude-test" }], system_prompt: "private model prompt" }],
      default_prompt_profile: "claude",
      allow_session_prompt: true,
      access: { allow_authenticated: true, allowed_principals: ["operator-1"], allowed_roles: ["operator"] },
    });
  });
});

function open(name: string) { fireEvent.click(screen.getByText(name)); }
function click(name: string) { fireEvent.click(screen.getByRole("button", { name })); }
function change(name: string, value: string) {
  const field = screen.getByLabelText(name, { selector: "input, textarea, select" });
  fireEvent.change(field, { target: { value } });
}
