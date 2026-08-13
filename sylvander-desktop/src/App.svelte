<script lang="ts">
  import crabMark from "../../docs/design/final-brand/sylvander-seed-crab-character-square.png";
  import { demoPlan, demoSessions, demoTasks, demoTranscript } from "./lib/demo";
  import type { ConnectionState, TranscriptEntry } from "./lib/types";

  let selectedId = $state(demoSessions[0].id);
  let query = $state("");
  let draft = $state("");
  let inspector = $state<"plan" | "tasks" | "changes">("plan");
  let inspectorOpen = $state(true);
  let approvalOpen = $state(true);
  let connection = $state<ConnectionState>("live");
  let transcript = $state<TranscriptEntry[]>([...demoTranscript]);
  let announcement = $state("Runtime connected");

  const selected = $derived(demoSessions.find((session) => session.id === selectedId)!);
  const filteredSessions = $derived(
    demoSessions.filter((session) =>
      `${session.label} ${session.workspace}`.toLowerCase().includes(query.toLowerCase()),
    ),
  );

  function selectSession(id: string) {
    selectedId = id;
    const session = demoSessions.find((item) => item.id === id);
    draft = session?.draft ?? "";
    announcement = `Loaded ${session?.label ?? "Session"}`;
  }

  function submit() {
    const text = draft.trim();
    if (!text || connection !== "live") return;
    transcript.push({ id: `user-${Date.now()}`, kind: "user", body: text });
    draft = "";
    announcement = "Goal submitted to the active Session";
  }

  function decide(approved: boolean) {
    approvalOpen = false;
    announcement = approved ? "Read access approved once" : "Tool request rejected";
  }

  function handleShortcut(event: KeyboardEvent) {
    if (!(event.metaKey || event.ctrlKey)) return;
    if (event.key.toLowerCase() === "k") {
      event.preventDefault();
      document.querySelector<HTMLInputElement>("#session-search")?.focus();
    }
    if (event.key.toLowerCase() === "n") {
      event.preventDefault();
      announcement = "New Session flow is ready for Runtime connection";
    }
  }
</script>

<svelte:window onkeydown={handleShortcut} />

<div class="app-shell">
  <nav class="product-rail" aria-label="Product">
    <div class="brand-mark"><img src={crabMark} alt="Sylvander Seed-Crab" /></div>
    <div class="rail-actions">
      <button class="rail-button active" aria-label="Work" aria-current="page"><span>◫</span></button>
      <button class="rail-button" aria-label="Agents"><span>◎</span></button>
      <button class="rail-button" aria-label="Automations"><span>⌁</span></button>
    </div>
    <button class="rail-button settings" aria-label="Settings"><span>⚙</span></button>
  </nav>

  <aside class="session-sidebar" aria-label="Sessions">
    <header class="sidebar-header">
      <div>
        <span class="eyebrow">Workspace</span>
        <h1>Sylvander</h1>
      </div>
      <button class="icon-button" aria-label="Create Session" title="New Session (⌘N)">＋</button>
    </header>

    <label class="session-search" for="session-search">
      <span aria-hidden="true">⌕</span>
      <input id="session-search" bind:value={query} placeholder="Find a Session" autocomplete="off" />
      <kbd>⌘K</kbd>
    </label>

    <div class="session-section-label"><span>Recent</span><span>{filteredSessions.length}</span></div>
    <div class="session-list">
      {#each filteredSessions as session (session.id)}
        <button
          class:active={session.id === selectedId}
          class="session-row"
          onclick={() => selectSession(session.id)}
          aria-current={session.id === selectedId ? "true" : undefined}
        >
          <span class:active={session.state === "active"} class:waiting={session.state === "waiting"} class="presence"></span>
          <span class="session-copy">
            <strong>{session.label}</strong>
            <span>{session.workspace}</span>
          </span>
          <time>{session.recency}</time>
        </button>
      {/each}
    </div>

    <footer class="runtime-card">
      <span class:live={connection === "live"} class="runtime-dot"></span>
      <div><strong>Local Runtime</strong><span>{connection === "live" ? "Connected" : connection}</span></div>
      <button aria-label="Runtime details">···</button>
    </footer>
  </aside>

  <main class="conversation" aria-label="Conversation">
    <header class="conversation-header">
      <div class="session-heading">
        <span class="presence active"></span>
        <div><h2>{selected.label}</h2><p>{selected.workspace}</p></div>
      </div>
      <div class="header-actions">
        <button class="quiet-button" onclick={() => (inspectorOpen = !inspectorOpen)} aria-pressed={inspectorOpen}>Plan <span>3/5</span></button>
        <button class="icon-button" aria-label="Session actions">···</button>
      </div>
    </header>

    <section class="transcript" aria-label="Transcript">
      <div class="session-intro">
        <span class="eyebrow">Agent workspace</span>
        <h3>Build with a clear trail from intent to evidence.</h3>
        <p>Agent <strong>sylvander</strong> · MiniMax-M2.7 · standard permissions</p>
      </div>

      {#each transcript as entry (entry.id)}
        <article class="turn {entry.kind}" data-status={entry.status}>
          <span class="turn-mark" aria-hidden="true">{entry.kind === "user" ? "❯" : entry.kind === "tool" ? "⎿" : "⏺"}</span>
          <div class="turn-content">
            {#if entry.title}<strong class="turn-title">{entry.title}</strong>{/if}
            <p>{entry.body}</p>
            {#if entry.meta}<span class="turn-meta">{entry.status === "verified" ? "✓ " : entry.status === "running" ? "↻ " : ""}{entry.meta}</span>{/if}
          </div>
          {#if entry.kind === "tool"}<button class="inspect-button" onclick={() => (inspectorOpen = true)}>Inspect</button>{/if}
        </article>
      {/each}
    </section>

    <div class="interaction-zone">
      {#if approvalOpen}
        <section class="decision-dock" aria-labelledby="approval-title">
          <div class="decision-icon" aria-hidden="true">◇</div>
          <div class="decision-copy">
            <span class="eyebrow">Approval · this Session</span>
            <h3 id="approval-title">Allow Read to inspect the desktop package?</h3>
            <p><code>Read</code> requests workspace read access for <code>sylvander-desktop/</code>.</p>
          </div>
          <div class="decision-actions">
            <button class="secondary-button" onclick={() => decide(false)}>Reject</button>
            <button class="primary-button" onclick={() => decide(true)}>Allow once</button>
          </div>
        </section>
      {/if}

      <form class="composer" onsubmit={(event) => { event.preventDefault(); submit(); }}>
        <label for="composer-input" class="sr-only">Message Sylvander</label>
        <textarea id="composer-input" bind:value={draft} rows="2" placeholder="What should we work through?" onkeydown={(event) => {
          if (event.key === "Enter" && !event.shiftKey) { event.preventDefault(); submit(); }
        }}></textarea>
        <div class="composer-footer">
          <div class="composer-tools">
            <button type="button" aria-label="Attach context">＋</button>
            <button type="button">Standard <span>⌄</span></button>
            <button type="button">MiniMax-M2.7 <span>⌄</span></button>
          </div>
          <div class="send-group"><span><kbd>↵</kbd> send · <kbd>⇧↵</kbd> line</span><button class="send-button" disabled={!draft.trim() || connection !== "live"} aria-label="Send">↑</button></div>
        </div>
      </form>
    </div>
  </main>

  {#if inspectorOpen}
    <aside class="inspector" aria-label="Session inspector">
      <header><div><span class="eyebrow">Live work</span><h2>Execution</h2></div><button class="icon-button" onclick={() => (inspectorOpen = false)} aria-label="Close inspector">×</button></header>
      <div class="inspector-tabs" role="tablist" aria-label="Execution details">
        {#each ["plan", "tasks", "changes"] as tab}
          <button role="tab" aria-selected={inspector === tab} class:active={inspector === tab} onclick={() => (inspector = tab as typeof inspector)}>{tab}</button>
        {/each}
      </div>
      {#if inspector === "plan"}
        <ol class="plan-list">
          {#each demoPlan as step, index}
            <li data-state={step.state}><span>{step.state === "complete" ? "✓" : index + 1}</span><p>{step.label}</p></li>
          {/each}
        </ol>
      {:else if inspector === "tasks"}
        <div class="task-list">{#each demoTasks as task}<article><span class="presence" class:active={task.state === "running"}></span><div><strong>{task.purpose}</strong><p>{task.owner} · {task.state}</p></div></article>{/each}</div>
      {:else}
        <div class="empty-inspector"><span>±</span><h3>No reviewable diff yet</h3><p>Runtime-owned changes will appear here.</p></div>
      {/if}
      <footer class="inspector-summary"><span>Context</span><strong>18.4k / 128k</strong><div><span style="width: 14%"></span></div></footer>
    </aside>
  {/if}

  <div class="sr-only" aria-live="polite">{announcement}</div>
</div>
