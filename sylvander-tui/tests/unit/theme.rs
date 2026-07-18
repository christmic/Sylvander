use super::*;
use crate::app::ToolStatus;

#[test]
fn status_mode_glyph_label_style_triple() {
    for m in [
        StatusMode::Connecting,
        StatusMode::Idle,
        StatusMode::Working,
        StatusMode::WaitingApproval,
        StatusMode::Asking,
        StatusMode::Disconnected,
    ] {
        let _ = m.glyph();
        let _ = m.label();
        let _ = m.style();
    }
    assert_eq!(StatusMode::Working.glyph(), "\u{25cc}");
    assert_eq!(StatusMode::Working.label(), "working");
    assert_eq!(StatusMode::Disconnected.glyph(), "!");
    assert_eq!(StatusMode::WaitingApproval.glyph(), "\u{25cf}");
    assert_eq!(StatusMode::Asking.label(), "asking");
}

#[test]
fn tool_status_glyph_three_distinct_styles() {
    let (g_pending, s_pending) = tool_status_glyph_and_style(ToolStatus::Pending);
    let (g_done, s_done) = tool_status_glyph_and_style(ToolStatus::Done);
    let (g_err, s_err) = tool_status_glyph_and_style(ToolStatus::Error);
    assert_eq!(g_pending, "\u{25cc}");
    assert_eq!(g_done, "\u{2713}");
    assert_eq!(g_err, "\u{2717}");
    assert_ne!(s_pending.fg, s_done.fg);
    assert_ne!(s_pending.fg, s_err.fg);
    assert_ne!(s_done.fg, s_err.fg);
}

#[test]
fn plan_step_three_states_distinct_glyphs() {
    let (g, s) = plan_step_glyph_and_style(true, false);
    assert_eq!(g, "\u{2713}");
    assert_eq!(s.fg, Some(TEAL));
    let (g, s) = plan_step_glyph_and_style(false, true);
    assert_eq!(g, "\u{25cf}");
    assert_eq!(s.fg, Some(BLUE));
    let (g, s) = plan_step_glyph_and_style(false, false);
    assert_eq!(g, "\u{25cb}");
    assert_eq!(s.fg, Some(RULE));
}

#[test]
fn compact_workspace_short_path_passes_through() {
    let p = std::path::PathBuf::from("/Users/christmix");
    assert_eq!(compact_workspace(&p, 60), "/Users/christmix");
}

#[test]
fn compact_workspace_long_path_collapses() {
    let p = std::path::PathBuf::from("/Users/christmix/OraculoSpace/Sylvander/sylvander-tui");
    let s = compact_workspace(&p, 25);
    assert!(
        s.starts_with("~/.../") || s.starts_with(".../"),
        "expected abbreviation, got: {s}",
    );
    assert!(s.contains("sylvander-tui"));
    assert!(s.chars().count() < p.display().to_string().chars().count());
}

#[test]
fn built_in_themes_have_distinct_semantic_palettes() {
    assert_ne!(
        palette_for(ThemeName::Sylvander),
        palette_for(ThemeName::Midnight)
    );
    assert_ne!(
        palette_for(ThemeName::Sylvander),
        palette_for(ThemeName::HighContrast)
    );
    assert_eq!(palette_for(ThemeName::Sylvander).canvas, BG);
}

#[test]
fn custom_foreground_and_accent_override_only_semantic_identity_roles() {
    let foreground = Color::Rgb(0xF1, 0xF2, 0xF3);
    let accent = Color::Rgb(0xB0, 0x80, 0xFF);
    let palette = resolved_palette(
        ThemeName::Sylvander,
        ColorCapability::TrueColor,
        ThemeOverrides {
            foreground: Some(foreground),
            accent: Some(accent),
        },
    );
    assert_eq!(palette.text, foreground);
    assert_eq!(palette.identity, accent);
    assert_eq!(palette.brand_violet, accent);
    assert_eq!(palette.active, accent);
    assert_eq!(palette.verified, SYLVANDER.verified);
    assert_eq!(palette.danger, SYLVANDER.danger);
}

#[test]
fn custom_color_parser_requires_six_digit_rgb() {
    assert_eq!(parse_color("#9B72FF"), Ok(Color::Rgb(0x9B, 0x72, 0xFF)));
    assert_eq!(parse_color("ece7de"), Ok(Color::Rgb(0xEC, 0xE7, 0xDE)));
    assert!(parse_color("violet").is_err());
    assert!(parse_color("#fff").is_err());
}

#[test]
fn terminal_color_detection_is_conservative_and_honors_opt_out() {
    assert_eq!(
        detect_color_capability(false, Some("xterm-256color"), None),
        ColorCapability::Ansi256
    );
    assert_eq!(
        detect_color_capability(false, Some("xterm"), Some("truecolor")),
        ColorCapability::TrueColor
    );
    assert_eq!(
        detect_color_capability(false, Some("xterm"), None),
        ColorCapability::Ansi16
    );
    assert_eq!(
        detect_color_capability(true, Some("xterm-256color"), Some("truecolor")),
        ColorCapability::Monochrome
    );
}

#[test]
fn capability_mapping_never_emits_rgb_beyond_truecolor() {
    let ansi256 = palette_for_capability(ThemeName::Sylvander, ColorCapability::Ansi256);
    assert!(matches!(ansi256.text, Color::Indexed(_)));
    assert!(matches!(ansi256.identity, Color::Indexed(_)));

    let ansi16 = palette_for_capability(ThemeName::Midnight, ColorCapability::Ansi16);
    assert_eq!(ansi16, HIGH_CONTRAST);
    assert!(!matches!(ansi16.text, Color::Rgb(..) | Color::Indexed(_)));

    let monochrome = palette_for_capability(ThemeName::Sylvander, ColorCapability::Monochrome);
    assert_eq!(monochrome.identity, Color::White);
    assert_ne!(monochrome.text, monochrome.canvas);
}

#[test]
fn every_built_in_theme_passes_semantic_contrast_validation() {
    for theme in [
        ThemeName::Sylvander,
        ThemeName::Midnight,
        ThemeName::HighContrast,
    ] {
        for capability in [
            ColorCapability::Monochrome,
            ColorCapability::Ansi16,
            ColorCapability::Ansi256,
            ColorCapability::TrueColor,
        ] {
            let palette = palette_for_capability(theme, capability);
            validate_palette(palette, capability)
                .unwrap_or_else(|error| panic!("{theme}/{capability}: {error}"));
        }
    }
}

#[test]
fn accessibility_fallbacks_preserve_static_hierarchy() {
    let base = Style::default();
    assert!(
        emphasis_for(base, false)
            .add_modifier
            .contains(Modifier::ITALIC)
    );
    assert!(
        emphasis_for(base, true)
            .add_modifier
            .contains(Modifier::UNDERLINED)
    );
    assert!(
        subtle_emphasis_for(base, true)
            .add_modifier
            .contains(Modifier::DIM)
    );
    assert!(cursor_for(true).add_modifier.contains(Modifier::REVERSED));
    assert!(
        cursor_for(false)
            .add_modifier
            .contains(Modifier::SLOW_BLINK)
    );
}

#[test]
fn theme_names_accept_documented_aliases() {
    assert_eq!(
        "default".parse::<ThemeName>().unwrap(),
        ThemeName::Sylvander
    );
    assert_eq!(
        "midnight".parse::<ThemeName>().unwrap(),
        ThemeName::Midnight
    );
    assert_eq!(
        "high-contrast".parse::<ThemeName>().unwrap(),
        ThemeName::HighContrast
    );
    assert!("unknown".parse::<ThemeName>().is_err());
}
