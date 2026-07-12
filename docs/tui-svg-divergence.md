# M-T17 SVG divergence table

Read every remaining SVG after M-T15. Below is what each draws, what I
implement, and the size of the gap. Priority ordered: P0 first
(visible bugs / user-facing), P1 next (state / state machine), P2+
after (motion / breakpoints / sidebar microinteractions).

| SVG | What it draws | Current build | Priority | Action |
|---|---|---|---|---|
| 01 | experience map | covered by §5 immersive | P2 | already OK |
| 02 | canonical immersive | done in M-T15 | — | OK |
| 03 | permission step-menu, AskUser multi-select w/ `f` key | Approval has y/n only; missing "Allow cargo test" scoped allow | **P0** | Approval modal needs choice list, not y/n |
| 04 | desktop sidebar | standalone TUI has no sidebar (per §10) | P2 | N/A — only relevant to Ghostty host |
| 05 | narrow `S ›` collapsed brand + 50-col responsive | I have generic responsive but no narrow brand collapse | P1 | narrow brand variant in compat |
| 06 | component spec: brand mark cell geometry + state language glyphs (◐ ● ✓ !) | I have theme::StatusMode but the cell spacing is inferred | P1 | verify cell margins, document exact spacing |
| 07 | large-scale session list (search + filters + status badges) | SessionsOverlay has filter but not the workspace: filter | P1 | add workspace + state filters |
| 08 | steer / queue / interrupt | not implemented | P0 | add DomainEvent::Steer, queue, interrupt |
| 09 | Permission Center (Pending / Session / Workspace / Global / History) | I have inline ApprovalModal only | P1 | add Permission Center panel |
| 10 | semantic transcript search | not implemented | P0 | add `Ctrl+R` / `Ctrl+F` search |
| 11 | CJK IME + attachments row | CJK not supported; attachments I have but layout differs (▣ vs ⎘) | P0 | add IME state + use ▣ glyph |
| 12 | reconnect + durable event queue | I have `Disconnected` mode only | P1 | add reconnecting + queued draft UI |
| 13 | primary journeys | not direct UI | P2 | N/A |
| 14 | focus precedence (6 layers) | I have modals but no explicit focus stack | P1 | add focus layer model |
| 15 | responsive matrix (4-column breakpoint table) | I have `Breakpoint` enum but matrix not asserted | P1 | add 5-column test |
| 16 | event-to-component handoff | mostly OK; some `WorkingStarted` not wired | P0 | wire working_active |
| 17 | turn rhythm + thinking placeholder | I have streaming text but no `⌁ Considering ...` placeholder | P1 | add thinking placeholder text |
| 18 | composer 7 states | mostly done; `f` for free-text in AskUser missing | P0 | add `f` key |
| 19 | sidebar microinteractions | standalone has no sidebar | P2 | N/A |
| 20 | overlay system (proportions) | popup centered, ~60% width, no `smallest scope selected by default` hint | P1 | add scope hint to ApprovalModal |
| 21 | sticky diff viewer | not implemented (no diff tool yet) | P2 | defer to M-T18 |
| 22 | motion timings (4-6 fps spinner, 30 fps coalesce, 500ms hold) | I have no animation timing | P2 | defer to M-T18 |

## P0 — ship-blockers user will notice in 5 minutes

1. **Approval modal step menu (SVG 03)** — `1. Allow once / 2. Allow cargo test in this workspace / 3. Reject with feedback`. Current y/n is too thin. Need choice-list modal.
2. **Steer / queue / interrupt (SVG 08)** — user wants to push follow-up turns while agent runs. Currently impossible.
3. **Search (SVG 10)** — `Ctrl+R` / `Ctrl+F` for transcript. Critical for long sessions.
4. **CJK IME (SVG 11)** — Chinese users need this. `▣` chip glyph + space select.
5. **Working wire event (SVG 16)** — `WorkingStarted` / `WorkingEnded`. Currently observational only.

## P1 — fix in this round if time allows

6. Narrow brand `S ›` (SVG 05)
7. Component spec spacing (SVG 06)
8. Sessions filter (SVG 07)
9. Permission Center UI (SVG 09)
10. Reconnect + queue (SVG 12)
11. Focus precedence (SVG 14)
12. Responsive matrix test (SVG 15)
13. Thinking placeholder (SVG 17)
14. Free-text `f` key in AskUser (SVG 18)
15. Approval scope hint (SVG 20)

## P2 — defer

- 04/19 sidebar (Ghostty-only)
- 13/21/22 (defer to M-T18)
