import { FormEvent, useEffect, useState } from "react";

import type { RuntimeIdentityBindingAction } from "./lib/gateway";
import type { RuntimeViewState } from "./lib/useRuntime";

interface IdentitySettingsProps {
  state: RuntimeViewState["identityBinding"];
  onClose(): void;
  onOpenProfile?: () => void;
  onClearChallenge(): void;
  onRequest(action: RuntimeIdentityBindingAction): Promise<boolean>;
}

export function IdentitySettings({ state, onClose, onOpenProfile, onClearChallenge, onRequest }: IdentitySettingsProps) {
  const [challengeId, setChallengeId] = useState("");
  const [proof, setProof] = useState("");
  const [unlinkArmed, setUnlinkArmed] = useState(false);
  const [copied, setCopied] = useState(false);
  const busy = state.status === "loading" || state.status === "submitting";

  useEffect(() => {
    if (state.status !== "submitting") {
      setChallengeId("");
      setProof("");
    }
  }, [state.status]);

  useEffect(() => {
    const challenge = state.challenge;
    if (!challenge) return;
    const remainingMs = challenge.expiresAtUnixSecs * 1_000 - Date.now();
    if (remainingMs <= 0) {
      onClearChallenge();
      return;
    }
    const timer = window.setTimeout(onClearChallenge, Math.min(remainingMs, 2_147_483_647));
    return () => window.clearTimeout(timer);
  }, [onClearChallenge, state.challenge]);

  function confirm(event: FormEvent) {
    event.preventDefault();
    if (!challengeId.trim() || !proof.trim()) return;
    void onRequest({
      operation: "confirm",
      challenge_id: challengeId.trim(),
      proof: proof.trim(),
    });
  }

  async function copyChallenge() {
    if (!state.challenge) return;
    try {
      await navigator.clipboard.writeText(`${state.challenge.id}\n${state.challenge.secret}`);
      setCopied(true);
    } catch {
      setCopied(false);
    }
  }

  return <aside className="inspector account-panel" aria-label="Account settings">
    <header><div><span className="eyebrow">Account security</span><h2>Identity Binding</h2></div><button className="icon-button" type="button" aria-label="Close account settings" onClick={onClose}>×</button></header>
    {onOpenProfile && <div className="inspector-tabs" role="tablist" aria-label="Account settings sections"><button role="tab" aria-selected="false" onClick={onOpenProfile}>profile</button><button role="tab" className="active" aria-selected="true">identity</button></div>}
    <section className="context-panel">
      <p>Runtime derives both sides from authenticated ingress. This client cannot choose a user, transport, Channel, or external principal.</p>
      {state.notice && <p role={state.status === "error" ? "alert" : "status"}>{state.notice}</p>}
      {state.status === "loading" && <p role="status">Resolving this authenticated identity…</p>}
      {state.binding && <article><h3>Linked as {state.binding.user_id}</h3><p>Binding revision {state.binding.revision} · linked {new Date(state.binding.linked_at_unix_secs * 1_000).toLocaleString()}</p></article>}
      {state.challenge && <section aria-labelledby="link-proof-title"><h3 id="link-proof-title">One-time external Channel proof</h3><p>Carry both values to the intended authenticated external Channel before {new Date(state.challenge.expiresAtUnixSecs * 1_000).toLocaleString()}.</p><label>Challenge<input aria-label="Issued challenge" readOnly value={state.challenge.id} /></label><label>One-time secret<input aria-label="Issued one-time secret" readOnly value={state.challenge.secret} /></label><button type="button" className="primary-button" onClick={() => void copyChallenge()}>{copied ? "Proof copied" : "Copy one-time proof"}</button></section>}
      <div className="context-actions"><button type="button" className="secondary-button" disabled={busy} onClick={() => void onRequest({ operation: "resolve" })}>Refresh binding</button><button type="button" className="primary-button" disabled={busy} onClick={() => void onRequest({ operation: "begin" })}>Link an external Channel</button></div>
      <form onSubmit={confirm}><fieldset><legend>Confirm a proof on this authenticated ingress</legend><label>Challenge<input aria-label="Challenge to confirm" autoComplete="off" maxLength={512} value={challengeId} onChange={(event) => setChallengeId(event.target.value)} /></label><label>One-time proof<input aria-label="One-time proof to confirm" autoComplete="off" maxLength={512} value={proof} onChange={(event) => setProof(event.target.value)} /></label><button className="primary-button" disabled={busy || !challengeId.trim() || proof.trim().length < 16}>Confirm identity link</button></fieldset></form>
      {state.binding && (unlinkArmed ? <button type="button" className="primary-button" disabled={busy} onClick={() => void onRequest({ operation: "unlink", expected_revision: state.binding!.revision })}>Confirm unlink revision {state.binding.revision}</button> : <button type="button" className="secondary-button" disabled={busy} onClick={() => setUnlinkArmed(true)}>Unlink this ingress…</button>)}
    </section>
  </aside>;
}
