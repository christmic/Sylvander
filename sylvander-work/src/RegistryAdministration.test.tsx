import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { RegistryAdministration } from "./RegistryAdministration";
import type { RuntimeViewState } from "./lib/useRuntime";

describe("RegistryAdministration", () => {
  it("builds typed Provider, Model, and Credential writes without secret values", () => {
    const request = vi.fn(async () => true);
    render(<RegistryAdministration state={{ status: "idle" }} onClose={() => undefined} onRequest={request} />);

    change("Provider id", "openai");
    change("Provider kind", "openai");
    change("Provider base URL", "https://api.openai.com");
    change("Credential binding id", "openai-primary");
    click("Add Provider features");
    change("Provider features 1", "responses");
    click("Create Provider");
    expect(request).toHaveBeenLastCalledWith({
      operation: "create_provider",
      provider_id: "openai",
      definition: {
        kind: "openai",
        features: ["responses"],
        base_url: "https://api.openai.com",
        credential_binding_id: "openai-primary",
      },
    });

    fireEvent.click(screen.getByRole("tab", { name: "model" }));
    change("Model Provider id", "openai");
    change("Model id", "gpt-test");
    change("Context window", "128000");
    change("Max output tokens", "8192");
    click("Add Model capabilities");
    change("Model capabilities 1", "tool_use");
    fireEvent.click(screen.getByRole("checkbox", { name: /Deprecated lifecycle/ }));
    change("Replacement model id", "gpt-next");
    fireEvent.click(screen.getByRole("checkbox", { name: /Configure pricing/ }));
    change("Input price micros", "1000000");
    change("Output price micros", "2000000");
    change("Cache write price micros", "500000");
    change("Cache read price micros", "100000");
    click("Create Model");
    expect(request).toHaveBeenLastCalledWith({
      operation: "create_model",
      provider_id: "openai",
      model_id: "gpt-test",
      definition: {
        context_window: 128000,
        max_output_tokens: 8192,
        capabilities: ["tool_use"],
        lifecycle: { status: "deprecated", replacement: "gpt-next" },
        pricing: {
          input_usd_micros_per_million: 1000000,
          output_usd_micros_per_million: 2000000,
          cache_write_usd_micros_per_million: 500000,
          cache_read_usd_micros_per_million: 100000,
        },
      },
    });

    fireEvent.click(screen.getByRole("tab", { name: "credential" }));
    change("Credential binding id", "openai-primary");
    change("Credential reference source", "file");
    change("Credential reference", "/run/secrets/openai");
    click("Create Credential binding");
    expect(request).toHaveBeenLastCalledWith({
      operation: "create_credential_binding",
      binding_id: "openai-primary",
      reference: { source: "file", path: "/run/secrets/openai" },
    });
    expect(screen.queryByLabelText(/secret value/i)).toBeNull();
  });

  it("requires explicit CAS confirmation before changing an active Provider revision", () => {
    const request = vi.fn(async () => true);
    const state: RuntimeViewState["registryAdministration"] = {
      status: "ready",
      provider: {
        id: "openai",
        activeRevision: 2,
        revisions: [providerRevision(2, true), providerRevision(1, false), providerRevision(3, false)],
      },
    };
    render(<RegistryAdministration state={state} onClose={() => undefined} onRequest={request} />);
    const actions = screen.getAllByRole("button", { name: "Make active…" });
    fireEvent.click(actions[0]);
    expect(request).not.toHaveBeenCalled();
    click("Confirm rollback from 2");
    expect(request).toHaveBeenCalledWith({
      operation: "rollback_provider_revision",
      provider_id: "openai",
      target_revision: 1,
      expected_active_revision: 2,
    });
  });
});

function providerRevision(revision: number, active: boolean) {
  return {
    definition: {
      provider_id: "openai", revision, kind: "openai", features: ["responses"],
      base_url_sha256: `sha256:base-${revision}`,
      credential_binding_id_sha256: "sha256:credential",
    },
    digest_sha256: `sha256:provider-${revision}`,
    created_at_unix_secs: revision,
    active,
  };
}

function click(name: string) { fireEvent.click(screen.getByRole("button", { name })); }
function change(name: string, value: string) {
  fireEvent.change(screen.getByLabelText(name, { selector: "input, select" }), { target: { value } });
}
