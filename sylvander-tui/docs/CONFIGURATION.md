# TUI Configuration and Themes

`TuiConfig` is the only startup configuration reader. Runtime, service, state,
and Panels receive resolved values and must not query environment variables.

## Environment settings

| Variable | Default | Range / values | Purpose |
|---|---:|---|---|
| `SYLVANDER_SOCKET` | `/tmp/sylvander.sock` | path | Unix Agent service socket; positional argument wins |
| `SYLVANDER_HISTORY_PATH` | `$XDG_CACHE_HOME/sylvander-tui/history.json` | path or empty | Composer history; empty disables persistence |
| `SYLVANDER_MODEL` | `—` | model label | Pre-connection fallback only; server runtime truth replaces it |
| `SYLVANDER_TUI_THEME` | `sylvander` | `sylvander`, `midnight`, `high-contrast` | Semantic color palette |
| `SYLVANDER_TUI_FOREGROUND` | theme value | six-digit RGB, for example `#ECE7DE` | Override primary message text |
| `SYLVANDER_TUI_ACCENT` | theme value | six-digit RGB, for example `#9B72FF` | Override identity, active, and Agent accent |
| `SYLVANDER_TUI_COLOR` | `auto` | `auto`, `none`, `ansi16`, `ansi256`, `truecolor` | Override detected terminal color capability |
| `SYLVANDER_TUI_EDITING` | `standard` | `standard`, `vim` | Composer editing style |
| `SYLVANDER_TUI_RENDER_FPS` | `60` | 5–120 | Maximum coalesced service render rate |
| `SYLVANDER_TUI_ANIMATION_MS` | `200` | 50–2000 | Low-frequency animation/status heartbeat |
| `SYLVANDER_TUI_RECONNECT_MS` | `1500` | 250–30000 | Retry interval after the Agent service disconnects |
| `SYLVANDER_TUI_MOUSE_SCROLL_LINES` | `4` | 1–40 | Transcript rows per mouse-wheel event |
| `SYLVANDER_TUI_KEY_SESSIONS` | `ctrl+p` | modified key chord | Open sessions |
| `SYLVANDER_TUI_KEY_TOOL_DETAILS` | `ctrl+o` | modified key chord | Toggle tool detail |
| `SYLVANDER_TUI_KEY_COMMANDS` | `ctrl+k` | modified key chord | Open command palette (`/` remains available) |
| `SYLVANDER_TUI_KEY_TRANSCRIPT_PAGE_UP` | `pageup` | key chord | Review older transcript rows |
| `SYLVANDER_TUI_KEY_TRANSCRIPT_PAGE_DOWN` | `pagedown` | key chord | Review newer transcript rows |
| `SYLVANDER_TUI_KEY_RETURN_LIVE` | `ctrl+end` | key chord | Return to live output |

Invalid values fail at startup with a concrete configuration error.
`auto` respects `NO_COLOR`, then detects truecolor from `COLORTERM`/`TERM`,
256 colors from `TERM`, and otherwise selects the conservative ANSI-16 palette.
The selected palette is checked for semantic text/status contrast at startup.
Foreground and accent overrides are mapped to the detected terminal color
capability and pass the same contrast validation. They do not replace verified,
waiting, or danger colors, so operational meaning remains stable.
Key names are case-insensitive and use `ctrl+`, `alt+`, or `shift+` modifiers.
Two actions cannot use the same chord. Unmodified printable global keys are
rejected, and printable chords require Ctrl or Alt, so custom bindings cannot
steal Composer input. Enter and Ctrl+C/Ctrl+X/Ctrl+Z are reserved. Text editing,
approval/question decisions, and `Esc`/`Ctrl+C` interruption remain fixed safety
contracts. `/help` and `/config` show the resolved bindings, not hard-coded
defaults.

`/config` opens the resolved configuration in the searchable, copyable
inspector. It reports the values captured at startup plus current server model,
workspace, and attachment limits; it does not reread the environment or mutate
configuration.

## Built-in themes

### `sylvander`

Pure-black canvas, warm-ivory prose, warm/violet Seed-Crab identity, blue active,
teal verified, amber waiting, and red failure.

### `midnight`

Blue-black canvas with cooler text and restrained violet/blue identity. Intended
for terminals where pure black produces excessive contrast.

### `high-contrast`

ANSI high-contrast roles for accessibility and limited color environments.

## Theme architecture

`theme::Palette` contains semantic roles:

- `canvas`, `text`, `text_dim`, `text_muted`
- `identity`, `brand_warm`, `brand_violet`
- `active`, `verified`, `waiting`, `danger`
- `rule`, `guide`

Panels call functions such as `theme::text()`, `theme::active()`, and
`theme::warning()`. They must not use `Color::Cyan`, RGB literals, or theme names.

To add a theme:

1. Add a `ThemeName` variant and parser spelling.
2. Define a complete `Palette` constant.
3. Return it from `palette_for`.
4. Add a palette test and visual snapshots for Welcome, conversation, overlay,
   and status states.
5. Document the new value in the table above.

Layout, symbols, and behavior must remain unchanged when a theme changes.
Limited-color terminals map the selected theme to their advertised capability;
monochrome mode keeps state distinguishable through labels, glyphs, and modifiers.
