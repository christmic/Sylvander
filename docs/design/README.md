# Sylvander Editable Design Artifacts

## Files

- `sylvander-tui-mockups.svg` — editable multi-artboard visual design.
- `sylvander-design-tokens.json` — shared visual tokens for the TUI and Ghostty shell.
- `../sylvander-tui-ux-design.md` — product and interaction source of truth.

## Import into Figma

1. Create or open a Figma design file.
2. Drag `sylvander-tui-mockups.svg` onto the canvas, or use **File → Place image**.
3. Ungroup the imported root once. Each artboard and major component has an SVG group ID such as `artboard-main`, `component-composer`, or `overlay-sessions`.
4. Replace the fallback font with a preferred monospace font. Recommended: Berkeley Mono, SF Mono, JetBrains Mono, or Geist Mono.
5. Create Figma color styles from `sylvander-design-tokens.json`. Token names match the intent used in the SVG.
6. Convert repeated groups—header, composer, status line, tool row, tab, and decision card—into Figma components.

SVG import preserves text, vectors, colors, borders, and groups. Figma may not preserve SVG group IDs in every import path; use layer position and the artboard labels if IDs are flattened.

## Artboards

1. **01 Main / Working** — canonical conversation, grouped tools, multiline composer, stable status.
2. **02 Decision / Approval** — risk-centered approval card over a dimmed transcript.
3. **03 Sessions / Tasks** — searchable session overlay and background task visibility.
4. **04 Ghostty Desktop** — native tabs hosting the same terminal UI without duplicating conversation chrome.
5. **05 Narrow / Responsive** — compact behavior below 80 columns.

## Editing rules

- Preserve the four persistent TUI regions: header, transcript, composer, status/help.
- Do not introduce a permanent sidebar at standard widths.
- Use coral for identity/selection, blue for active work, teal for verified success, amber for waiting/approval, and red for failure/destruction.
- Keep routine tool rows compact. Detail belongs in expandable rows or an inspection layer.
- Treat Ghostty tabs as views over durable sessions; closing a tab must not visually imply deletion.
- Test every state in monochrome and with an ASCII-symbol fallback.

## Handoff

The SVG expresses layout and visual hierarchy, not pixel-perfect terminal metrics. Implementation should map spacing to terminal cells and use the nearest supported colors for the detected terminal capability. Behavioral requirements in the design report take precedence over accidental SVG geometry.
