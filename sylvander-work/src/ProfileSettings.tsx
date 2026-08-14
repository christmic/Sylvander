import { FormEvent, useEffect, useState } from "react";

import type { RuntimePrivacyClass, RuntimeUserProfileAction, RuntimeUserProfileData, RuntimeUserProfileExport, RuntimeUserProfileView } from "./lib/gateway";
import type { RuntimeViewState } from "./lib/useRuntime";

interface ProfileSettingsProps {
  state: RuntimeViewState["userProfile"];
  onClose(): void;
  onOpenIdentity?: () => void;
  onRequest(action: RuntimeUserProfileAction): Promise<boolean>;
  onSaveExport(exported: RuntimeUserProfileExport): Promise<boolean>;
}

type OptionalEnum<T extends string> = "" | T;

export function ProfileSettings({ state, onClose, onOpenIdentity, onRequest, onSaveExport }: ProfileSettingsProps) {
  const [language, setLanguage] = useState("");
  const [languagePrivacy, setLanguagePrivacy] = useState<RuntimePrivacyClass>("personal");
  const [locale, setLocale] = useState("");
  const [localePrivacy, setLocalePrivacy] = useState<RuntimePrivacyClass>("personal");
  const [detail, setDetail] = useState<OptionalEnum<"concise" | "balanced" | "detailed">>("");
  const [detailPrivacy, setDetailPrivacy] = useState<RuntimePrivacyClass>("personal");
  const [tone, setTone] = useState<OptionalEnum<"direct" | "warm" | "formal">>("");
  const [tonePrivacy, setTonePrivacy] = useState<RuntimePrivacyClass>("personal");
  const [accessEnabled, setAccessEnabled] = useState(false);
  const [accessPrivacy, setAccessPrivacy] = useState<RuntimePrivacyClass>("sensitive");
  const [screenReader, setScreenReader] = useState(false);
  const [reduceMotion, setReduceMotion] = useState(false);
  const [highContrast, setHighContrast] = useState(false);
  const [constraints, setConstraints] = useState<Array<{ value: string; privacy: RuntimePrivacyClass }>>([]);
  const [deleteArmed, setDeleteArmed] = useState(false);
  const [exportStatus, setExportStatus] = useState<"saving" | "saved" | "cancelled" | "error">();
  const profile = state.profile;
  const busy = state.status === "loading" || state.status === "submitting";

  useEffect(() => loadEditor(profile), [profile?.revision]);
  useEffect(() => setExportStatus(undefined), [state.export?.exported_at_unix_secs]);

  function loadEditor(current?: RuntimeUserProfileView) {
    const data = current?.profile;
    setLanguage(data?.preferred_language?.value ?? "");
    setLanguagePrivacy(data?.preferred_language?.privacy_class ?? "personal");
    setLocale(data?.locale?.value ?? "");
    setLocalePrivacy(data?.locale?.privacy_class ?? "personal");
    setDetail(data?.response_detail?.value ?? "");
    setDetailPrivacy(data?.response_detail?.privacy_class ?? "personal");
    setTone(data?.communication_tone?.value ?? "");
    setTonePrivacy(data?.communication_tone?.privacy_class ?? "personal");
    setAccessEnabled(Boolean(data?.accessibility));
    setAccessPrivacy(data?.accessibility?.privacy_class ?? "sensitive");
    setScreenReader(data?.accessibility?.value.screen_reader_optimized ?? false);
    setReduceMotion(data?.accessibility?.value.reduce_motion ?? false);
    setHighContrast(data?.accessibility?.value.high_contrast ?? false);
    setConstraints(data?.constraints.map((item) => ({
      value: item.value,
      privacy: item.privacy_class,
    })) ?? []);
    setDeleteArmed(false);
  }

  function profileData(): RuntimeUserProfileData {
    const preferredLanguage = language.trim();
    const localeId = locale.trim();
    return {
      ...(preferredLanguage ? {
        preferred_language: { value: preferredLanguage, privacy_class: languagePrivacy },
      } : {}),
      ...(localeId ? { locale: { value: localeId, privacy_class: localePrivacy } } : {}),
      ...(detail ? { response_detail: { value: detail, privacy_class: detailPrivacy } } : {}),
      ...(tone ? { communication_tone: { value: tone, privacy_class: tonePrivacy } } : {}),
      ...(accessEnabled ? {
        accessibility: {
          value: {
            screen_reader_optimized: screenReader,
            reduce_motion: reduceMotion,
            high_contrast: highContrast,
          },
          privacy_class: accessPrivacy,
        },
      } : {}),
      constraints: constraints
        .map((item) => ({ value: item.value.trim(), privacy_class: item.privacy }))
        .filter((item) => item.value),
    };
  }

  function save(event: FormEvent) {
    event.preventDefault();
    const action: RuntimeUserProfileAction = profile
      ? { operation: "update", expected_revision: profile.revision, profile: profileData() }
      : { operation: "create", profile: profileData() };
    void onRequest(action);
  }

  function updateConstraint(index: number, patch: Partial<(typeof constraints)[number]>) {
    setConstraints((current) => current.map((item, itemIndex) => itemIndex === index
      ? { ...item, ...patch }
      : item));
  }

  async function downloadExport() {
    if (!state.export) return;
    setExportStatus("saving");
    try {
      setExportStatus(await onSaveExport(state.export) ? "saved" : "cancelled");
    } catch {
      setExportStatus("error");
    }
  }

  return <aside className="inspector account-panel" aria-label="Account settings">
    <header><div><span className="eyebrow">Owner settings</span><h2>User Profile</h2></div><button className="icon-button" type="button" aria-label="Close account settings" onClick={onClose}>×</button></header>
    {onOpenIdentity && <div className="inspector-tabs" role="tablist" aria-label="Account settings sections"><button role="tab" className="active" aria-selected="true">profile</button><button role="tab" aria-selected="false" onClick={onOpenIdentity}>identity</button></div>}
    <section className="context-panel">
      <p>Runtime derives the profile owner from this authenticated connection. Values stay out of Session history and local storage.</p>
      {state.notice && <p role={state.status === "error" ? "alert" : "status"}>{state.notice}</p>}
      {state.status === "loading" ? <p role="status">Loading owner profile…</p> : <form onSubmit={save}>
        <label>Preferred language<input aria-label="Preferred language" maxLength={64} value={language} onChange={(event) => setLanguage(event.target.value)} /></label>
        <PrivacySelect name="Language privacy" value={languagePrivacy} onChange={setLanguagePrivacy} disabled={!language} />
        <label>Locale<input aria-label="Locale" maxLength={64} value={locale} onChange={(event) => setLocale(event.target.value)} /></label>
        <PrivacySelect name="Locale privacy" value={localePrivacy} onChange={setLocalePrivacy} disabled={!locale} />
        <label>Response detail<select aria-label="Response detail" value={detail} onChange={(event) => setDetail(event.target.value as typeof detail)}><option value="">not set</option><option value="concise">concise</option><option value="balanced">balanced</option><option value="detailed">detailed</option></select></label>
        <PrivacySelect name="Response detail privacy" value={detailPrivacy} onChange={setDetailPrivacy} disabled={!detail} />
        <label>Communication tone<select aria-label="Communication tone" value={tone} onChange={(event) => setTone(event.target.value as typeof tone)}><option value="">not set</option><option value="direct">direct</option><option value="warm">warm</option><option value="formal">formal</option></select></label>
        <PrivacySelect name="Communication tone privacy" value={tonePrivacy} onChange={setTonePrivacy} disabled={!tone} />
        <fieldset><legend>Accessibility</legend><label><input type="checkbox" checked={accessEnabled} onChange={(event) => setAccessEnabled(event.target.checked)} /> Store accessibility preferences</label><label><input type="checkbox" disabled={!accessEnabled} checked={screenReader} onChange={(event) => setScreenReader(event.target.checked)} /> Screen-reader optimized</label><label><input type="checkbox" disabled={!accessEnabled} checked={reduceMotion} onChange={(event) => setReduceMotion(event.target.checked)} /> Reduce motion</label><label><input type="checkbox" disabled={!accessEnabled} checked={highContrast} onChange={(event) => setHighContrast(event.target.checked)} /> High contrast</label><PrivacySelect name="Accessibility privacy" value={accessPrivacy} onChange={setAccessPrivacy} disabled={!accessEnabled} /></fieldset>
        <fieldset><legend>Interaction constraints · {constraints.length}/16</legend>{constraints.map((item, index) => <div key={index}><input aria-label={`Constraint ${index + 1}`} maxLength={512} value={item.value} onChange={(event) => updateConstraint(index, { value: event.target.value })} /><PrivacySelect name={`Constraint ${index + 1} privacy`} value={item.privacy} onChange={(privacy) => updateConstraint(index, { privacy })} /><button type="button" className="secondary-button" onClick={() => setConstraints((current) => current.filter((_, itemIndex) => itemIndex !== index))}>Remove constraint {index + 1}</button></div>)}<button type="button" className="secondary-button" disabled={constraints.length >= 16} onClick={() => setConstraints((current) => [...current, { value: "", privacy: "sensitive" }])}>Add constraint</button></fieldset>
        <div className="context-actions"><button className="primary-button" disabled={busy}>{profile ? "Save profile" : "Create profile"}</button>{profile && <button type="button" className="secondary-button" disabled={busy} onClick={() => void onRequest({ operation: "correct", expected_revision: profile.revision, profile: profileData() })}>Record correction</button>}</div>
      </form>}
      {profile && <div className="context-actions"><button type="button" className="secondary-button" disabled={busy} onClick={() => void onRequest({ operation: "set_do_not_learn", expected_revision: profile.revision, enabled: !profile.do_not_learn })}>{profile.do_not_learn ? "Allow learning" : "Do not learn"}</button><button type="button" className="secondary-button" disabled={busy} onClick={() => void onRequest({ operation: "export", format: "json" })}>Prepare JSON export</button>{deleteArmed ? <button type="button" className="primary-button" disabled={busy} onClick={() => void onRequest({ operation: "delete", expected_revision: profile.revision })}>Confirm profile deletion</button> : <button type="button" className="secondary-button" disabled={busy} onClick={() => setDeleteArmed(true)}>Delete profile…</button>}</div>}
      {state.export && <button type="button" className="primary-button" disabled={exportStatus === "saving"} onClick={() => void downloadExport()}>Save JSON export…</button>}
      {exportStatus && exportStatus !== "saving" && <p role={exportStatus === "error" ? "alert" : "status"}>{exportStatus === "saved" ? "Profile export saved." : exportStatus === "cancelled" ? "Profile export was not saved." : "Profile export could not be saved."}</p>}
    </section>
  </aside>;
}

function PrivacySelect({ name, value, disabled, onChange }: { name: string; value: RuntimePrivacyClass; disabled?: boolean; onChange(value: RuntimePrivacyClass): void }) {
  return <label>{name}<select aria-label={name} value={value} disabled={disabled} onChange={(event) => onChange(event.target.value as RuntimePrivacyClass)}><option value="personal">personal</option><option value="sensitive">sensitive</option><option value="restricted">restricted</option></select></label>;
}
