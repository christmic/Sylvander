# Sylvander Editable Design Artifacts

## Files

- `01-experience-map.svg` — product-level surface and flow map.
- `02-tui-immersive.svg` — canonical immersive conversation screen.
- `03-interaction-states.svg` — detailed plan, approval, AskUser, diff, command, paste, and history states.
- `04-ghostty-sidebar.svg` — Ghostty desktop with left session sidebar.
- `05-responsive-recovery.svg` — narrow, first-run, disconnected, and recovery states.
- `06-component-spec.svg` — brand mark and component anatomy.
- `07-session-management.svg` — large-scale sessions, notifications, multi-window, and draft conflicts.
- `08-execution-control.svg` — steer, queue, interrupt, and non-interruptible work.
- `09-permission-center.svg` — permission scopes, pending decisions, rules, and invalidation.
- `10-transcript-navigation.svg` — search, checkpoints, forks, context, compaction, model, and mode.
- `11-composer-ime.svg` — Chinese IME, CJK editing, attachments, mentions, history, and draft recovery.
- `12-resilience-operations.svg` — reconnect, diagnostics, performance degradation, trust, and verification.
- `13-primary-journeys.svg` — start/resume, plan, permission, interrupt, reconnect, and fork journeys.
- `14-interaction-contract.svg` — blocking focus, shortcut ownership, and state ownership.
- `15-responsive-accessibility.svg` — terminal-cell breakpoints and capability fallbacks.
- `16-event-component-handoff.svg` — event/component lifecycle and replayable design QA.
- `17-turn-rhythm.svg` — immersive live, settled, and inspected turn density.
- `18-composer-interactions.svg` — idle, focus, multiline, steer, paste, IME, and adaptive status.
- `19-sidebar-microinteractions.svg` — priority sections, hover, peek, search, and collapsed rail.
- `20-overlay-system.svg` — Approval, AskUser, Command Palette, and Session Switcher.
- `21-diff-artifact-viewer.svg` — sticky Diff, Command Output, image, file, and artifact inspection.
- `22-motion-focus.svg` — timing, focus language, unread/live return, toast, and reduced motion.
- `23-agent-brand-exploration.svg` — four agent-brand logo directions and recommended Vector Core concept.
- `24-agent-brand-system.svg` — selected wordmark, app icon, terminal, monochrome, small-size, and multi-agent variants.
- `25-brand-applied-tui.svg` — current brand applied to Welcome, TUI conversation, and Ghostty Sidebar.
- `26-brand-refinement.svg` — refinement from generic container shape to final Convergence Core.
- `27-convergence-brand-tui.svg` — normative final brand applied to entry, turns, subagents, and Ghostty workspace.
- `07-session-management.svg` — large session collections, notification policy, linked views, and draft conflict.
- `08-execution-control.svg` — steer, queue, interrupt boundaries, and non-interruptible work.
- `09-permission-center.svg` — pending decisions, scoped permission rules, audit history, and revocation.
- `10-transcript-navigation.svg` — transcript search, checkpoints, forks, context, and compaction.
- `11-composer-ime.svg` — IME composition, mentions, attachments, templates, and draft recovery.
- `12-resilience-operations.svg` — reconnect, crash ownership, diagnostics, safe mode, and degraded performance.
- `sylvander-design-tokens.json` — shared visual tokens for the TUI and Ghostty shell.
- `../sylvander-tui-ux-design.md` — product and interaction source of truth.

## Import into Figma

1. Create or open a Figma design file.
2. Drag the numbered SVG files onto the canvas, or use **File → Place image**.
3. Ungroup each imported root once. Each artboard and major component has a named SVG group ID.
4. Replace the fallback font with a preferred monospace font. Recommended: Berkeley Mono, SF Mono, JetBrains Mono, or Geist Mono.
5. Create Figma color styles from `sylvander-design-tokens.json`. Token names match the intent used in the SVG.
6. Convert repeated groups—header, composer, status line, tool row, tab, and decision card—into Figma components.

SVG import preserves text, vectors, colors, borders, and groups. Figma may not preserve SVG group IDs in every import path; use layer position and the artboard labels if IDs are flattened.

## Design levels

1. **Experience map** — system ownership, navigation, and cross-surface relationships.
2. **Core TUI** — immersive conversation, grouped tools, multiline composer, and status.
3. **Interaction states** — detailed decisions and secondary workflows.
4. **Ghostty desktop** — persistent left session sidebar hosting the same terminal UI.
5. **Responsive/recovery** — compact and exceptional states.
6. **Component spec** — reusable anatomy, state, logo, and spacing references.
7. **Session system** — scale, combined state, notification, and linked views.
8. **Execution control** — active-turn message semantics and cancellation boundaries.
9. **Permission system** — durable policy and auditable decisions.
10. **Navigation/context** — long-session retrieval, branching, and context lifecycle.
11. **Advanced input** — multilingual composition and durable drafting.
12. **Operations** — failure ownership, recovery, diagnostics, performance, and trust.
13. **Journeys** — end-to-end transition, recovery, and exit behavior.
14. **Interaction contract** — deterministic focus and state boundaries.
15. **Responsive/accessibility** — measurable cell layouts and fallbacks.
16. **Implementation handoff** — protocol-to-component mapping and QA cases.
17. **Turn rhythm** — immersive transcript density and progressive disclosure.
18. **Composer** — input-state visual and micro-interaction specification.
19. **Sidebar detail** — desktop session navigation behavior and density.
20. **Overlay system** — common visual language for temporary interaction layers.
21. **Inspection** — code, output, and generated artifact viewing.
22. **Motion/focus** — perceptual timing and stable feedback behavior.
23. **Brand exploration** — alternative concepts and selection rationale.
24. **Brand system** — production variants and semantic brand language.
25. **Brand application** — latest normative visual direction for product surfaces.
26. **Brand refinement** — final silhouette and conceptual convergence.
27. **Final application** — production reference for branded TUI and multi-agent UI.
7. **Session management** — scale, attention, linked views, and multi-window consistency.
8. **Execution control** — steering, queued prompts, interruption, and safe boundaries.
9. **Permission center** — decision lifecycle, reusable scopes, revocation, and audit.
10. **Transcript navigation** — search, checkpoints, forks, context, and compaction.
11. **Composer and IME** — mixed-width editing, completion, attachments, and draft conflicts.
12. **Resilience and operations** — subsystem failures, recovery, diagnostics, and trust signals.

## Editing rules

- Preserve the four persistent TUI regions: header, transcript, composer, status/help.
- Do not introduce a permanent sidebar at standard widths.
- Use coral for identity/selection, blue for active work, teal for verified success, amber for waiting/approval, and red for failure/destruction.
- Do not place conversation turns or routine tool output inside filled gray cards.
- Keep routine tool rows compact. Use alignment, indentation, whitespace, and restrained guide lines.
- Treat Ghostty sidebar items as views over durable sessions; hiding a view must not visually imply deletion.
- Test every state in monochrome and with an ASCII-symbol fallback.

## Handoff

The SVG expresses layout and visual hierarchy, not pixel-perfect terminal metrics. Implementation should map spacing to terminal cells and use the nearest supported colors for the detected terminal capability. Behavioral requirements in the design report take precedence over accidental SVG geometry.
