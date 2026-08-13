import { useState } from "react";

import type { RuntimeCredentialSecretReference, RuntimeModelDefinitionDraft, RuntimeProviderDefinitionDraft, RuntimeRegistryAdminRequest } from "./lib/gateway";
import type { RuntimeViewState } from "./lib/useRuntime";

interface RegistryAdministrationProps {
  state: RuntimeViewState["registryAdministration"];
  onClose(): void;
  onRequest(request: RuntimeRegistryAdminRequest): Promise<boolean>;
}

export function RegistryAdministration({ state, onClose, onRequest }: RegistryAdministrationProps) {
  const [section, setSection] = useState<"provider" | "model" | "credential">("provider");
  return <aside className="inspector registry-panel" aria-label="Registry administration">
    <header><div><span className="eyebrow">Privileged control plane</span><h2>Runtime Registry</h2></div><button className="icon-button" type="button" aria-label="Close Registry administration" onClick={onClose}>×</button></header>
    <div className="inspector-tabs" role="tablist" aria-label="Registry sections">{(["provider", "model", "credential"] as const).map((item) => <button key={item} role="tab" className={section === item ? "active" : ""} aria-selected={section === item} onClick={() => setSection(item)}>{item}</button>)}</div>
    <section className="context-panel">
      <p>Inspection exposes only digests and lifecycle metadata. Every write form is complete and write-only; credential fields accept references, never secret values.</p>
      {state.notice && <p role={state.status === "error" ? "alert" : "status"}>{state.notice}</p>}
      {section === "provider" && <ProviderRegistry state={state} onRequest={onRequest} />}
      {section === "model" && <ModelRegistry state={state} onRequest={onRequest} />}
      {section === "credential" && <CredentialRegistry state={state} onRequest={onRequest} />}
    </section>
  </aside>;
}

function ProviderRegistry({ state, onRequest }: SectionProps) {
  const [providerId, setProviderId] = useState(state.provider?.id ?? "");
  const [kind, setKind] = useState("");
  const [features, setFeatures] = useState<string[]>([]);
  const [baseUrl, setBaseUrl] = useState("");
  const [bindingId, setBindingId] = useState("");
  const active = state.provider?.id === providerId ? state.provider.activeRevision : undefined;
  const definition: RuntimeProviderDefinitionDraft = { kind: kind.trim(), features: clean(features), base_url: baseUrl.trim(), credential_binding_id: bindingId.trim() };
  return <div className="registry-section"><h3>Provider revisions</h3><div className="definition-grid"><Field name="Provider id" value={providerId} set={setProviderId} /><Field name="Provider kind" value={kind} set={setKind} /><Field name="Provider base URL" value={baseUrl} set={setBaseUrl} /><Field name="Credential binding id" value={bindingId} set={setBindingId} /></div><StringEditor label="Provider features" values={features} set={setFeatures} /><div className="context-actions"><button type="button" className="secondary-button" disabled={!providerId || busy(state)} onClick={() => void onRequest({ operation: "list_provider_revisions", provider_id: providerId.trim(), limit: 50 })}>Load Provider</button>{active === undefined ? <button type="button" className="primary-button" disabled={!providerId || !kind || !baseUrl || !bindingId || busy(state)} onClick={() => void onRequest({ operation: "create_provider", provider_id: providerId.trim(), definition })}>Create Provider</button> : <button type="button" className="primary-button" disabled={!kind || !baseUrl || !bindingId || busy(state)} onClick={() => void onRequest({ operation: "stage_provider_revision", provider_id: providerId.trim(), revision: active + 1, expected_active_revision: active, definition })}>Stage Provider revision {active + 1}</button>}</div><RevisionCards values={state.provider?.id === providerId ? state.provider.revisions : []} active={active} identity={(item) => item.definition.revision} summary={(item) => `${item.definition.kind} · ${item.definition.features.join(", ") || "no features"} · base ${item.definition.base_url_sha256} · credential ${item.definition.credential_binding_id_sha256}`} onChange={(revision, rollback) => active !== undefined && void onRequest(rollback ? { operation: "rollback_provider_revision", provider_id: providerId, target_revision: revision, expected_active_revision: active } : { operation: "activate_provider_revision", provider_id: providerId, revision, expected_active_revision: active })} /></div>;
}

function ModelRegistry({ state, onRequest }: SectionProps) {
  const [providerId, setProviderId] = useState(state.model?.providerId ?? "");
  const [modelId, setModelId] = useState(state.model?.modelId ?? "");
  const [contextWindow, setContextWindow] = useState("0");
  const [maxOutput, setMaxOutput] = useState("0");
  const [capabilities, setCapabilities] = useState<string[]>([]);
  const [deprecated, setDeprecated] = useState(false);
  const [replacement, setReplacement] = useState("");
  const [pricing, setPricing] = useState(false);
  const [inputPrice, setInputPrice] = useState("0");
  const [outputPrice, setOutputPrice] = useState("0");
  const [cacheWritePrice, setCacheWritePrice] = useState("");
  const [cacheReadPrice, setCacheReadPrice] = useState("");
  const selected = state.model?.providerId === providerId && state.model.modelId === modelId ? state.model : undefined;
  const active = selected?.activeRevision;
  const definition: RuntimeModelDefinitionDraft = { context_window: Number(contextWindow), max_output_tokens: Number(maxOutput), capabilities: clean(capabilities), lifecycle: deprecated ? { status: "deprecated", ...(replacement.trim() ? { replacement: replacement.trim() } : {}) } : { status: "active" }, ...(pricing ? { pricing: { input_usd_micros_per_million: Number(inputPrice), output_usd_micros_per_million: Number(outputPrice), ...(cacheWritePrice ? { cache_write_usd_micros_per_million: Number(cacheWritePrice) } : {}), ...(cacheReadPrice ? { cache_read_usd_micros_per_million: Number(cacheReadPrice) } : {}) } } : {}) };
  const configured = providerId.trim() && modelId.trim() && Number(contextWindow) > 0 && Number(maxOutput) > 0;
  return <div className="registry-section"><h3>Model revisions</h3><div className="definition-grid"><Field name="Model Provider id" value={providerId} set={setProviderId} /><Field name="Model id" value={modelId} set={setModelId} /><NumberField name="Context window" value={contextWindow} set={setContextWindow} /><NumberField name="Max output tokens" value={maxOutput} set={setMaxOutput} /></div><StringEditor label="Model capabilities" values={capabilities} set={setCapabilities} /><label><input type="checkbox" checked={deprecated} onChange={(event) => setDeprecated(event.target.checked)} /> Deprecated lifecycle</label>{deprecated && <Field name="Replacement model id" value={replacement} set={setReplacement} />}<label><input type="checkbox" checked={pricing} onChange={(event) => setPricing(event.target.checked)} /> Configure pricing</label>{pricing && <div className="definition-grid"><NumberField name="Input price micros" value={inputPrice} set={setInputPrice} /><NumberField name="Output price micros" value={outputPrice} set={setOutputPrice} /><NumberField name="Cache write price micros" value={cacheWritePrice} set={setCacheWritePrice} /><NumberField name="Cache read price micros" value={cacheReadPrice} set={setCacheReadPrice} /></div>}<div className="context-actions"><button type="button" className="secondary-button" disabled={!providerId || !modelId || busy(state)} onClick={() => void onRequest({ operation: "list_model_revisions", provider_id: providerId.trim(), model_id: modelId.trim(), limit: 50 })}>Load Model</button>{active === undefined ? <button type="button" className="primary-button" disabled={!configured || busy(state)} onClick={() => void onRequest({ operation: "create_model", provider_id: providerId.trim(), model_id: modelId.trim(), definition })}>Create Model</button> : <button type="button" className="primary-button" disabled={!configured || busy(state)} onClick={() => void onRequest({ operation: "stage_model_revision", provider_id: providerId.trim(), model_id: modelId.trim(), revision: active + 1, expected_active_revision: active, definition })}>Stage Model revision {active + 1}</button>}</div><RevisionCards values={selected?.revisions ?? []} active={active} identity={(item) => item.definition.revision} summary={(item) => `${item.definition.context_window} context · ${item.definition.max_output_tokens} output · ${item.definition.capabilities.join(", ")} · pricing ${item.definition.pricing_sha256 ?? "not set"}`} onChange={(revision, rollback) => active !== undefined && void onRequest(rollback ? { operation: "rollback_model_revision", provider_id: providerId, model_id: modelId, target_revision: revision, expected_active_revision: active } : { operation: "activate_model_revision", provider_id: providerId, model_id: modelId, revision, expected_active_revision: active })} /></div>;
}

function CredentialRegistry({ state, onRequest }: SectionProps) {
  const [bindingId, setBindingId] = useState(state.credential?.bindingId ?? "");
  const [source, setSource] = useState<"environment" | "file">("environment");
  const [referenceValue, setReferenceValue] = useState("");
  const selected = state.credential?.bindingId === bindingId ? state.credential : undefined;
  const active = selected?.activeGeneration;
  const reference: RuntimeCredentialSecretReference = source === "environment"
    ? { source, name: referenceValue.trim() }
    : { source, path: referenceValue.trim() };
  return <div className="registry-section"><h3>Credential generations</h3><p>Enter an environment variable name or file path. Secret values are not accepted by this protocol.</p><div className="definition-grid"><Field name="Credential binding id" value={bindingId} set={setBindingId} /><label>Reference source<select aria-label="Credential reference source" value={source} onChange={(event) => setSource(event.target.value as typeof source)}><option value="environment">environment</option><option value="file">file</option></select></label><Field name="Credential reference" value={referenceValue} set={setReferenceValue} /></div><div className="context-actions"><button type="button" className="secondary-button" disabled={!bindingId || busy(state)} onClick={() => void onRequest({ operation: "list_credential_generations", binding_id: bindingId.trim(), limit: 50 })}>Load Credential</button>{active === undefined ? <button type="button" className="primary-button" disabled={!bindingId || !referenceValue || busy(state)} onClick={() => void onRequest({ operation: "create_credential_binding", binding_id: bindingId.trim(), reference })}>Create Credential binding</button> : <button type="button" className="primary-button" disabled={!referenceValue || busy(state)} onClick={() => void onRequest({ operation: "stage_credential_generation", binding_id: bindingId.trim(), generation: active + 1, expected_active_generation: active, reference })}>Stage Credential generation {active + 1}</button>}</div>{selected?.bindingIdSha256 && <p>Binding digest {selected.bindingIdSha256}</p>}<RevisionCards noun="Generation" values={selected?.generations ?? []} active={active} identity={(item) => item.generation} summary={(item) => `${item.reference_kind} reference · ${item.reference_digest_sha256} · configured ${item.reference_configured ? "yes" : "no"}`} onChange={(generation, rollback) => active !== undefined && void onRequest(rollback ? { operation: "rollback_credential_generation", binding_id: bindingId, target_generation: generation, expected_active_generation: active } : { operation: "activate_credential_generation", binding_id: bindingId, generation, expected_active_generation: active })} /></div>;
}

interface SectionProps {
  state: RuntimeViewState["registryAdministration"];
  onRequest(request: RuntimeRegistryAdminRequest): Promise<boolean>;
}

function RevisionCards<T>({ values, active, identity, summary, onChange, noun = "Revision" }: { values: T[]; active?: number; identity(value: T): number; summary(value: T): string; onChange(revision: number, rollback: boolean): void; noun?: string }) {
  const [armed, setArmed] = useState<number>();
  return <div>{values.map((value) => {
    const revision = identity(value);
    return <article key={revision} className="admin-revision"><h3>{noun} {revision}{revision === active ? " · active" : ""}</h3><p>{summary(value)}</p>{revision !== active && (armed === revision ? <button type="button" className="primary-button" onClick={() => onChange(revision, active !== undefined && revision < active)}>Confirm {active !== undefined && revision < active ? "rollback" : "activation"} from {active}</button> : <button type="button" className="secondary-button" disabled={active === undefined} onClick={() => setArmed(revision)}>Make active…</button>)}</article>;
  })}</div>;
}

function StringEditor({ label, values, set }: { label: string; values: string[]; set(values: string[]): void }) {
  return <fieldset><legend>{label}</legend>{values.map((value, index) => <div key={index} className="definition-row"><input aria-label={`${label} ${index + 1}`} value={value} onChange={(event) => set(values.map((item, itemIndex) => itemIndex === index ? event.target.value : item))} /><button type="button" className="secondary-button" aria-label={`Remove ${label} ${index + 1}`} onClick={() => set(values.filter((_, itemIndex) => itemIndex !== index))}>Remove</button></div>)}<button type="button" className="secondary-button" onClick={() => set([...values, ""])}>Add {label}</button></fieldset>;
}

function Field({ name, value, set }: { name: string; value: string; set(value: string): void }) { return <label>{name}<input aria-label={name} value={value} onChange={(event) => set(event.target.value)} /></label>; }
function NumberField({ name, value, set }: { name: string; value: string; set(value: string): void }) { return <label>{name}<input aria-label={name} type="number" min="0" value={value} onChange={(event) => set(event.target.value)} /></label>; }
function clean(values: string[]) { return [...new Set(values.map((value) => value.trim()).filter(Boolean))]; }
function busy(state: RuntimeViewState["registryAdministration"]) { return state.status === "loading" || state.status === "submitting"; }
