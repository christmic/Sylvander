# Sylvander UI Design — Current Index

This directory contains only the current Sylvander UI design system. Historical
and rejected TUI boards were removed because they contradicted the implemented
black-canvas, Seed-Crab direction.

## Source of truth

1. [`../sylvander-tui-ux-design.md`](../sylvander-tui-ux-design.md) — normative
   TUI layout, transcript, composer, responsive, and visual rules.
2. [`../sylvander-brand-system.md`](../sylvander-brand-system.md) — normative
   product character and brand behavior.
3. [`sylvander-design-tokens.json`](sylvander-design-tokens.json) — machine-readable
   colors and terminal-cell measurements.

When an SVG differs from the Markdown specification, the Markdown specification
wins. When implementation differs from both, it is a bug until the specification
is deliberately revised.

## Editable UI layout boards

These SVGs remain editable geometry and hierarchy references. Some boards still
contain the earlier transcript glyphs `›` and `◆`; those glyphs are historical
and non-normative. Current conversation rows use `❯` for the user and `⏺` for
the Agent. Selection cursors may still use `›`, and decision titles may still
use `◆`. The Markdown specification and implementation own that distinction
until the boards are redrawn.

- [`tui-01-welcome.svg`](tui-01-welcome.svg) — session entry and persistent
  transcript prelude.
- [`tui-02-conversation.svg`](tui-02-conversation.svg) — user turn, clean Agent
  response, list typography, and Composer.
- [`tui-03-responsive.svg`](tui-03-responsive.svg) — standard, fullscreen, narrow,
  and multiline Composer behavior.
- [`tui-04-decision-dock.svg`](tui-04-decision-dock.svg) — approval and Agent
  question states that temporarily replace the Composer.
- [`tui-05-focus-picker.svg`](tui-05-focus-picker.svg) — command, model, permission,
  file, and single-session resume selection.
- [`tui-06-review-view.svg`](tui-06-review-view.svg) — plan acceptance, explicit
  plan editing, diff inspection, and rollback confirmation.
- [`tui-07-interaction-responsive.svg`](tui-07-interaction-responsive.svg) — wide,
  standard, and narrow temporary-surface behavior.

The SVGs use editable text and vector layers and can be imported into Figma.

## Approved brand assets

- `final-brand/sylvander-seed-crab-character-source.png` — approved raster source.
- `final-brand/sylvander-seed-crab-character-square.png` — square-mask source.
- `final-brand/sylvander-seed-crab-character-faithful.svg` — detailed vector.
- `final-brand/sylvander-seed-crab-master.png` — rendered master.
- `final-brand/sylvander-logo-system.png` — logo application reference.

The Welcome character is an authored terminal adaptation of these sources. A
compact Agent-turn marker is a presence mark, not a fallback logo.

## Current non-negotiable rules

- The default `sylvander` theme canvas is pure black (`#000000`). Alternate
  configured themes may substitute their validated semantic canvas.
- Ordinary transcript content has no filled card or gray container.
- Main content is anchored at the first terminal column; fullscreen width
  adds space on the right and never recenters the transcript.
- Welcome is the first transcript block. Sending a message appends below it;
  it disappears only through normal scrolling.
- The compact half-area Seed-Crab appears once at session entry. Its linear
  dimensions remain near 70% so the complete character does not collapse into
  a triangular shell mark.
- User turns begin with `❯`. Agent turns begin with one violet `⏺` presence
  mark. The former three-line `/\\ (••) <__>` reply marker is prohibited.
- Agent prose wraps on word boundaries. Markdown control characters are not
  shown as raw decoration.
- Composer rules span the terminal width. The `❯` prompt touches the same left
  edge as those rules and grows upward from one row.
- Session/model/branch/tool state lives in the bottom status row.
- The standalone TUI owns one active session. `/resume` is a temporary picker;
  only a Ghostty host may provide persistent multi-session navigation.
- Decisions use a bottom Decision Dock, selectors use a bottom Focus Picker, and
  long content uses a Review View. Generic centered dialogs are prohibited.

## Removed material

The former numbered `01–27` SVG series, the old ASCII system, implementation
audits, divergence tables, and stale verification reports are intentionally not
archived here. They specified rejected logos, gray canvases, centered fullscreen
layouts, top headers, old Composer copy, or the rejected three-line reply face.
They must not be restored as normative references.
