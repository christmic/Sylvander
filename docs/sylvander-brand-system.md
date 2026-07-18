# Sylvander Agent Character & Brand System

> Status: Final character and product-symbol direction approved
>
> Version: 4.0
>
> Date: 2026-07-12
>
> SSOT: This document is the normative source for Sylvander's product character, symbol, wordmark, motion, and in-product brand behavior.

## 1. Design decision

Sylvander uses an **anthropomorphic Seed–Crab–Core product character**: a compact agent companion that appears to notice, think, act, coordinate, protect, and return with evidence.

It is not a decorative mascot and not a diagram of an agent pipeline. The character is a product participant. Its expression and motion communicate real interface state while its silhouette creates brand recognition.

The final character master is `design/final-brand/sylvander-seed-crab-master.png`. Its seed shell, crab action body, paired luminous core eyes, left warm/right calm duality, sprout, and connection orbit are canonical. `design/final-brand/sylvander-seed-crab-character-faithful.svg` is its high-fidelity editable vector derivative. Current terminal application is specified by `sylvander-tui-ux-design.md` and the editable `design/tui-*.svg` boards; it does not redefine the brand character. Rejected abstract marks are removed from the current design directory.

### 1.1 Canonical formula

`Seed + Crab + Core = Sylvander`

- **Seed:** life, growth, hope, companionship, and durable value.
- **Crab:** exploration, lateral awareness, tool use, protection, and multi-task execution.
- **Core:** intelligence, connection, system thinking, and calm focus.
- **Warm left half:** companionship, trust, initiative, and growth.
- **Calm right half:** reasoning, data, coordination, and technical depth.
- **Paired light eyes:** sensitivity and rationality acting together; never a single surveillance eye.

## 2. Who Sylvander is

### 2.1 Role

Sylvander is the capable resident agent of a developer workspace. It receives an ambiguous intention, studies the environment, coordinates tools or specialist agents, advances the work, and checks the result before returning.

Its relationship to the user is **trusted co-worker**, not servant, pet, oracle, or superhero.

### 2.2 Personality coordinates

| Axis | Sylvander | Avoid |
|---|---|---|
| Intelligence | Observant and deliberate | All-knowing or mystical |
| Confidence | Quietly assured | Cocky or overeager |
| Warmth | Present and humane | Cute, childish, or needy |
| Energy | Alert, economical motion | Hyperactive bouncing |
| Humor | Dry, occasional | Meme-driven personality |
| Agency | Proactive with visible boundaries | Acting without consent |
| Collaboration | Coordinates a team naturally | Commanding a swarm |

Five words must survive every expression: **observant, composed, capable, curious, accountable**.

### 2.3 Anthropomorphic model

Sylvander should feel alive through four cues, in order of importance:

1. **Attention** — it can visibly orient toward the user, work, or a pending decision.
2. **Posture** — its compact body can open, lean, settle, or brace without becoming anatomical.
3. **Rhythm** — restrained timing suggests thought and intention.
4. **Expression** — minimal eye or aperture changes convey state; no cartoon mouth is required.

Anthropomorphism comes from behavior, not from adding a human face to a geometric logo.

## 3. Character anatomy

The character must be constructed from a small, stable vocabulary so it remains recognizable in vectors, pixels, and terminal cells.

### 3.1 Required parts

| Part | Function | Constraint |
|---|---|---|
| Core body | Stable silhouette and identity | One compact mass; no generic circle-in-badge |
| Attention aperture | Eye-like focus and expression | One aperture or paired points; never photorealistic |
| Two side gestures | Reach, inspect, hold, or coordinate | Abstract appendages; not literal human arms |
| Ground/notch | Gives posture and direction | Must not turn into a speech bubble or location pin |

The body may carry a subtle trace of woodland or crab-like intelligence—protective shell, lateral awareness, careful reach—but it must not become a literal crab, animal costume, or fantasy creature.

### 3.2 Silhouette rules

- Recognizable as a filled, single-color silhouette at 16 px.
- Distinguishable from a sparkle, chatbot bubble, brain, eye, robot head, command prompt, and corporate hexagon.
- Stable front-facing resting pose; state changes must not destroy its identity.
- No more than three dominant exterior gestures.
- Negative space must remain open at terminal-cell scale.
- It must look intentional when completely still.
- It must have an asymmetric detail or posture so it does not feel like a generic app icon.

### 3.3 Face and expression rules

- The attention aperture is the primary expressive feature.
- A mouth is omitted in the default pose. If explored, it may appear only as a one-stroke state accent and must not smile continuously.
- Avoid eyebrows, cheeks, teeth, pupils with highlights, and emoji-like expressions.
- Emotion is subtle: focus, curiosity, caution, relief, and readiness—not joy, sadness, anger, or panic.
- The character never looks distressed during ordinary execution; errors communicate seriousness through posture and color, not suffering.

## 4. Product-state performance

The character must earn its place by making the agent's state easier to perceive.

| Product state | Character performance | Motion principle | Terminal fallback |
|---|---|---|---|
| Ready | Open, balanced, attending to composer | One quiet arrival and settle | Neutral compact mark |
| Listening | Aperture turns toward user input | Single orientation change | Mark + `listening` |
| Thinking | Gaze shifts inward; body becomes still | Slow 3–5 frame attention loop | `·` pulse beside mark |
| Acting | One side gesture reaches toward work | Directional motion, no spinning | Mark + active verb |
| Coordinating | Side gestures acknowledge two or more agents | Alternating, not simultaneous noise | Main mark with small hollow satellites |
| Waiting for user | Open posture faces composer | Motion stops after one cue | Coral attention accent + text |
| Approval required | Braced, attentive, neutral | No pulse that pressures consent | Character beside explicit decision |
| Verified complete | Body settles; aperture opens | One short resolve, then still | Teal check remains separate |
| Recoverable error | Slightly closed or tilted | One interruption, no shaking | Error symbol + explanation |
| Disconnected | Character dims but retains silhouette | Fade once; no endless blinking | Outline/low-contrast mark |

Color never carries state alone. Text and conventional symbols remain responsible for accessibility and precision.

## 5. A family, not clones

Sylvander is the principal character. Subagents are related presences, not miniature copies with different names.

- **Main agent:** complete body and attention aperture.
- **Subagent:** reduced outline or seed form derived from the main silhouette.
- **Active subagent:** one state accent plus role label.
- **Agent team:** main character accompanied by two or three quiet seeds; never a row of cartoon faces.
- **User:** never represented as another Sylvander character.

Role, status, and ownership must remain readable without color or animation.

## 6. Brand rendering system

The identity has four coordinated levels. Each level is designed independently rather than mechanically shrinking a large illustration.

### 6.1 Hero character

Used only for first-run onboarding, an empty workspace, and selected brand material.

- Target size: 64–160 px or 6–10 terminal rows.
- May show the full pose and restrained entrance animation.
- Must leave more empty space than occupied space.
- Never placed inside a gray card merely to make it visible.

### 6.2 Product mark

Used for app icon, session entry, Ghostty native session-rail header, and
loading surface.

- Target size: 16–48 px.
- Uses the canonical silhouette with at most one internal aperture.
- Must work in one flat color.
- No word, `S`, arrow, sparkle, or container is added to explain it.

### 6.3 Terminal character

Used in the TUI at 2–4 rows high. It is a deliberate character-cell interpretation, not auto-traced vector art.

- Unicode and ASCII variants are separately drawn.
- Wide and narrow terminal variants are specified.
- Ambiguous-width glyphs are avoided in the default fallback.
- The terminal pose may simplify appendages but must preserve body, attention, and posture.

### 6.4 Presence glyph

Used beside an active response, session, or agent status.

- One cell whenever possible; two cells maximum.
- It indicates identity, not completion or severity.
- Standard symbols such as `✓`, `!`, and `?` remain separate state indicators.
- It must not be repeated on every paragraph.

## 7. Wordmark relationship

`SYLVANDER` is a calm counterweight to the living character.

- Mixed case `Sylvander` is preferred in conversational product UI.
- Uppercase `SYLVANDER` is reserved for the formal lockup and short entry moment.
- The wordmark must not imitate terminal block lettering merely because the product runs in a terminal.
- Letterforms should feel literary-technical: open counters, controlled width, one distinctive custom detail.
- Character and wordmark may appear side by side; the character never replaces a letter in the name.
- Descriptor: `agent workspace`, used only when context does not already explain the product.

## 8. Color contract

The character has one identity color, supported by neutral canvas and semantic UI colors. The final hue will be selected after silhouette testing, not before it.

| Role | Rule |
|---|---|
| Identity | One primary hue for the character; consistent across states |
| Canvas | Warm near-black or terminal background; never required for recognition |
| Text | Terminal default or warm ivory |
| Active | Cyan/blue may describe execution but does not recolor the whole character |
| Decision | Coral/amber stays near the decision, not inside the brand by default |
| Verified | Teal/green is paired with a check and evidence |
| Error | Red is reserved for errors and never becomes a character emotion |

No three-color logo, rainbow gradient, permanent glow, or color-dependent facial expression.

## 9. Motion contract

Motion should make Sylvander feel intentional rather than decorative.

- Resting state is truly still.
- A state transition has one readable action, then settles.
- Default loops last at least 1.2 seconds and vary no more than one small feature.
- No bounce, rubber easing, continuous rotation, breathing scale, confetti, or typing dots inside the face.
- Startup animation must complete within 900 ms and never block input.
- Reduced-motion mode renders the resolved pose immediately.
- Narrow terminals may omit hero animation entirely.
- Animation frames are authored and tested as terminal art, following Codex's size-aware principle; they are not video converted to text.

## 10. Placement in the TUI

### Session entry

The character appears once, establishes attention, and yields to the task.

```text
  [seed-crab]  Sylvander
               ~/workspace · model · permission mode

  What should we work through?
```

The terminal rendition deliberately simplifies the approved seed-crab while preserving its split shell, paired eyes, and two claw-leaves.

### Conversation

- No gray message boxes around ordinary user or agent output.
- The character marks the beginning of a meaningful agent turn, not every streamed line.
- During long execution, activity language appears inline while the character remains compact.
- Tool results use tool semantics, not character expressions.
- Completion pairs a conventional check with evidence; the character does not perform celebration.

### Ghostty multi-session workspace

- The native left session rail uses the presence glyph in the Sylvander header
  and active-session row.
- Waiting, running, approval, error, and complete states use separate symbols and labels.
- The hero character never occupies the session rail.
- Multiple sessions do not create multiple animated faces; only the focused session may animate.

## 11. Reference synthesis

Sylvander learns principles from leading terminal agents without borrowing their form.

| Reference | Principle learned | Sylvander application |
|---|---|---|
| Codex | Identity can be temporal; animation disappears when space is limited | A short, size-aware character entrance and restrained state frames |
| Qwen Code | A terminal brand needs authored width tiers and immediate recognition | Separate hero, terminal, and narrow variants rather than automatic scaling |
| Kimi Code | A tiny recurring symbol can feel present without consuming the session | Compact daily character paired with useful session context |
| Claude Code | Brand should recede during focused conversation | Character establishes presence, then content remains dominant |

## 12. Prohibited directions

- Agent pipeline, convergence arrows, nodes, routing diagrams, and workflow metaphors as the logo.
- Generic robot head, chat bubble, sparkle, eye, brain, terminal prompt, or hexagon.
- Literal crab mascot, human body, fantasy elf, animal costume, or chibi proportions.
- A face added to an otherwise generic geometric badge.
- Three simultaneous brand colors.
- Huge permanent ASCII banner inside every resumed session.
- Character reactions that trivialize approvals, destructive actions, errors, or user interruption.
- Different silhouettes for different states.
- A visual concept whose meaning requires a paragraph to explain.

## 13. Production fidelity gate

Every derivative asset must pass all of these tests:

1. **Silhouette:** identifiable among ten monochrome agent marks at 16 px.
2. **Character:** observers independently describe at least three target traits from section 2.2.
3. **Category:** reads as an agent presence, not infrastructure or a chat application.
4. **Terminal:** recognizable in 1-cell, 2–4-row, and 6–10-row versions.
5. **Stillness:** feels alive while completely static.
6. **Motion:** ready, thinking, acting, waiting, and resolved states share one anatomy.
7. **Immersion:** ordinary transcript remains visually dominant.
8. **Accessibility:** works in monochrome, reduced motion, and common dark/light terminal themes.
9. **Originality:** does not resemble the signature mark or character of Codex, Qwen, Kimi, Claude, or another major agent.
10. **Affection without cuteness:** users can form attachment without the product becoming childish.

Assets that fail this gate are exploratory and cannot replace the approved master.

## 14. Exploration record

`Convergence Core`, `Vector Core`, and the earlier `◖S◗` shell mark are rejected explorations. None may be treated as the current Sylvander logo.

Their former SVG boards were removed from `docs/design/` so rejected work cannot be mistaken for current UI guidance. Repository history remains the only archive.

## 15. Approved assets

- [`design/final-brand/sylvander-seed-crab-master.png`](./design/final-brand/sylvander-seed-crab-master.png) — canonical detailed pet/agent character master.
- [`design/final-brand/sylvander-logo-system.png`](./design/final-brand/sylvander-logo-system.png) — rendered logo, icon, terminal, scale, and avatar study.
- [`design/final-brand/sylvander-seed-crab-character-faithful.svg`](./design/final-brand/sylvander-seed-crab-character-faithful.svg) — high-fidelity multicolor vector derivative of the approved character.
- [`design/final-brand/sylvander-seed-crab-character-source.png`](./design/final-brand/sylvander-seed-crab-character-source.png) — approved transparent raster source.
- [`design/final-brand/sylvander-seed-crab-character-square.png`](./design/final-brand/sylvander-seed-crab-character-square.png) — square-mask application source.
- [`design/tui-01-welcome.svg`](./design/tui-01-welcome.svg) — current editable terminal-character application.
- [`design/tui-02-conversation.svg`](./design/tui-02-conversation.svg) — current turn-presence application.
