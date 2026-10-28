//! Terminal font resolution — keeping icon glyphs on an icon font.
//!
//! A configured family such as `Monospace` carries no Nerd-Font glyph, so
//! every icon a shell prompt or a vim statusline draws misses the primary
//! font and lands in Pango's generic fallback. That fallback is fontconfig's
//! global sort, and it is won by whichever installed font claims the Private
//! Use Area codepoint — which is rarely the icon font. On a box carrying the
//! Arphic CJK fonts, `AR PL UMing HK` answers for U+E0A0 (powerline branch),
//! U+E5FA (Seti file icons) and U+E700 (devicons), so lualine paints a row of
//! unrelated ideographs where its icons belong.
//!
//! Pango walks every family in a description's comma-separated list before it
//! reaches that generic fallback, so naming an installed Nerd Font as a
//! secondary family repairs the icons while leaving the text font the user
//! chose in charge of every glyph it actually covers.

use crate::config::Config;
use gtk4 as gtk;
use gtk4::pango::FontDescription;
use gtk4::prelude::*;
use std::sync::OnceLock;

/// Substring every Nerd Font family name carries.
const NERD_FONT_MARKER: &str = "Nerd Font";

/// The preferred family in `DEFAULT_FONT_DESC`; keep saved settings unchanged.
const DEFAULT_FONT_FAMILY: &str = "JetBrainsMono Nerd Font Mono";

/// Resolve only our single default family. Custom names and ordered lists belong
/// to the user, including lists that happen to contain the default family.
fn default_family_fallback(family: &str, default_available: bool) -> &str {
    if !default_available && family.eq_ignore_ascii_case(DEFAULT_FONT_FAMILY) {
        "Monospace"
    } else {
        family
    }
}

fn runtime_family(family: &str) -> &str {
    if !family.eq_ignore_ascii_case(DEFAULT_FONT_FAMILY) || !gtk4::is_initialized_main_thread() {
        return family;
    }
    // Font enumeration is expensive: like icon detection, do it once after GTK
    // starts, never caching a headless/unit-test answer. Use the display's Pango
    // family catalog rather than load_font(), which silently substitutes a face.
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    let available = *AVAILABLE.get_or_init(|| {
        gtk4::Label::new(None)
            .pango_context()
            .list_families()
            .iter()
            .any(|candidate| candidate.name().eq_ignore_ascii_case(DEFAULT_FONT_FAMILY))
    });
    default_family_fallback(family, available)
}

/// Rank `name` as an icon fallback; lower is better, `None` rejects it.
///
/// The `Mono` cuts hold every icon to a single cell, which is the only shape
/// that lines up in a terminal grid; the proportional cuts overhang their
/// neighbours. Nerd Fonts also ships icon-only `Symbols` cuts that exist to be
/// somebody else's fallback, so those win over borrowing icons out of a full
/// text font that may disagree about metrics.
fn icon_family_rank(name: &str) -> Option<u8> {
    let variant = if name.ends_with("Nerd Font Mono") {
        0
    } else if name.ends_with("Nerd Font Propo") {
        4
    } else if name.ends_with(NERD_FONT_MARKER) {
        2
    } else {
        return None;
    };
    Some(variant + u8::from(!name.starts_with("Symbols")))
}

/// Best icon fallback among `families`, or `None` when none is installed.
///
/// Ties break on the family name so the choice cannot drift between runs with
/// the order fontconfig happens to enumerate in.
pub(crate) fn select_icon_family<I>(families: I) -> Option<String>
where
    I: IntoIterator<Item = String>,
{
    families
        .into_iter()
        .filter_map(|name| icon_family_rank(&name).map(|rank| (rank, name)))
        .min()
        .map(|(_, name)| name)
}

/// The icon family this process falls back to, resolved once.
///
/// Enumerating families walks every font file fontconfig knows about, and the
/// answer cannot change while the process runs.
fn detected_icon_family() -> Option<&'static str> {
    static DETECTED: OnceLock<Option<String>> = OnceLock::new();
    if let Some(detected) = DETECTED.get() {
        return detected.as_deref();
    }
    // Listing families needs a font map, which needs GTK up on this thread —
    // and unit tests reach the CSS builder with no toolkit at all. Answer
    // `None` there without memoizing it, so a verdict taken before GTK existed
    // cannot outlive the moment.
    if !gtk4::is_initialized_main_thread() {
        return None;
    }
    // Any widget hands out a context bound to the display's font map; an
    // unparented label needs neither a window nor realization.
    let context = gtk4::Label::new(None).pango_context();
    let detected = select_icon_family(
        context
            .list_families()
            .iter()
            .map(|family| family.name().to_string()),
    );
    DETECTED.get_or_init(|| detected).as_deref()
}

/// The icon family to append for `config`: the configured override, the
/// detected default, or `None` when the user turned the fallback off.
pub(crate) fn icon_family(config: &Config) -> Option<&str> {
    match config.icon_font.as_deref() {
        Some(name) if name.is_empty() || name.eq_ignore_ascii_case("none") => None,
        Some(name) => Some(name),
        None => detected_icon_family(),
    }
}

/// Extend a Pango family list with `icon_family`, or `None` to leave it alone.
///
/// A description that already names an icon font — or names nothing, and so
/// asks for the display default — is returned untouched: appending there would
/// change which font draws ordinary text, not just the icons.
fn family_list_with_icon(families: &str, icon_family: &str) -> Option<String> {
    if families.trim().is_empty() {
        return None;
    }
    let already_covered = families.split(',').any(|family| {
        let family = family.trim();
        family.contains(NERD_FONT_MARKER) || family.eq_ignore_ascii_case(icon_family)
    });
    // set_family receives a literal Pango list, not a parsed font description:
    // whitespace here becomes part of the appended family name and can make
    // an installed fallback unresolvable. CSS formatting is separate below.
    (!already_covered).then(|| format!("{families},{icon_family}"))
}

/// Parse `desc`, resolve a missing default, and append the icon fallback.
///
/// Everything else in the description — size, weight, style — survives, so the
/// user's font string stays the one thing that decides how text looks.
pub(crate) fn font_description(desc: &str, icon_family: Option<&str>) -> FontDescription {
    let mut font = FontDescription::from_string(desc);
    if let Some(family) = font.family() {
        let resolved = runtime_family(&family);
        if resolved != family.as_str() {
            font.set_family(resolved);
        }
    }
    let Some(icon_family) = icon_family else {
        return font;
    };
    let families = font.family().unwrap_or_default();
    if let Some(list) = family_list_with_icon(&families, icon_family) {
        font.set_family(&list);
    }
    font
}

/// The description every VTE surface should be given for `desc` under `config`.
pub(crate) fn terminal_font_description(desc: &str, config: &Config) -> FontDescription {
    font_description(desc, icon_family(config))
}

/// The CSS `font-family` value for `family`, with the icon fallback behind it.
///
/// Block chrome draws command text through GTK labels rather than the VTE, so
/// it needs the same fallback list spelled the way CSS wants it.
pub(crate) fn css_font_stack(family: &str, icon_family: Option<&str>) -> String {
    let family = runtime_family(family);
    let escape = |value: &str| value.replace('\\', "\\\\").replace('"', "\\\"");
    let primary = format!("\"{}\"", escape(family));
    match icon_family {
        Some(icon) if family_list_with_icon(family, icon).is_some() => {
            format!("{primary}, \"{}\"", escape(icon))
        }
        _ => primary,
    }
}

/// The same effective size for GTK text and VTE's configured font × zoom.
/// Pango owns parsing: styled names, fractional points and absolute pixels must
/// not take a separate, rounded path through the chrome stylesheet.
pub(crate) fn effective_font_size_css(desc: &str, scale: f64) -> String {
    let mut font = FontDescription::from_string(desc);
    if font.size() <= 0 {
        font = FontDescription::from_string(crate::config::DEFAULT_FONT_DESC);
    }
    let size = f64::from(font.size()) / f64::from(gtk::pango::SCALE);
    let scale = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    let unit = if font.is_size_absolute() { "px" } else { "pt" };
    format!("{}{unit}", size * scale)
}

/// Scale authored UI roles relative to the existing nominal 14pt default.
/// Resolve both sizes in the same live context: Pango owns point/pixel and
/// display resolution, so neither a guessed DPI nor a device-scale factor is
/// applied here. The caller passes the already-frozen independent chrome size.
pub(crate) fn reference_ui_role_multiplier(
    chrome_size: &str,
    chrome_scale: f64,
    context: &gtk::pango::Context,
) -> f64 {
    reference_size_role_multiplier(chrome_size, "14", chrome_scale, context)
}

/// Rich Content follows the same Main size and scale as its native PTY.
/// Preserve the authored hierarchy at Config's actual default size; the
/// reference capture's explicitly configured 11pt is not the product default.
pub(crate) fn reference_content_role_multiplier(
    content_desc: &str,
    content_scale: f64,
    context: &gtk::pango::Context,
) -> f64 {
    reference_size_role_multiplier(
        content_desc,
        crate::config::DEFAULT_FONT_DESC,
        content_scale,
        context,
    )
}

fn reference_size_role_multiplier(
    configured_desc: &str,
    nominal_desc: &str,
    requested_scale: f64,
    context: &gtk::pango::Context,
) -> f64 {
    use gtk::pango::prelude::FontExt;

    let scale = if requested_scale.is_finite() && requested_scale > 0.0 {
        requested_scale
    } else {
        1.0
    };
    let set_size = |font: &mut FontDescription, size: &FontDescription| {
        if size.size() > 0 {
            if size.is_size_absolute() {
                font.set_absolute_size(f64::from(size.size()));
            } else {
                font.set_size(size.size());
            }
        }
    };
    let mut nominal = context
        .font_description()
        .unwrap_or_else(|| FontDescription::from_string("sans-serif"));
    set_size(&mut nominal, &FontDescription::from_string(nominal_desc));
    let mut configured = nominal.clone();
    set_size(
        &mut configured,
        &FontDescription::from_string(configured_desc),
    );
    let resolved = |description: &FontDescription| {
        context
            .load_font(description)
            .map(|font| font.describe_with_absolute_size().size())
            .filter(|size| *size > 0)
            .map(f64::from)
    };
    match (resolved(&configured), resolved(&nominal)) {
        (Some(configured), Some(nominal)) => scale * configured / nominal,
        // Retain the prior role scale until a normal appearance update has a
        // usable font context; do not invent a point-to-pixel conversion.
        _ => scale,
    }
}

/// Edit one property without losing the user's size unit, style or weight.
pub(crate) fn font_description_with_family(desc: &str, family: &str) -> String {
    let mut font = FontDescription::from_string(desc);
    font.set_family(family);
    font.to_string()
}

/// Settings edits keep absolute pixels absolute and fractional points fractional.
pub(crate) fn font_description_with_size(desc: &str, size: f64) -> String {
    let mut font = FontDescription::from_string(desc);
    if size.is_finite() && size > 0.0 {
        let scaled = size.clamp(6.0, 72.0) * f64::from(gtk::pango::SCALE);
        if font.is_size_absolute() {
            font.set_absolute_size(scaled);
        } else {
            font.set_size(scaled.round() as i32);
        }
    }
    font.to_string()
}

/// A size-only Pango description preserves absolute pixels and fractional points.
pub(crate) fn unscaled_font_size(desc: &str) -> String {
    effective_font_size_css(desc, 1.0)
        .trim_end_matches("pt")
        .to_owned()
}

/// Scope the shared text size to this application's window. Target text nodes
/// directly so nested captions, pane metadata and theme heading classes do not
/// compound relative sizes. Image sizing and the decorative connection dot keep
/// their existing geometry. Newly created tabs/rows inherit this provider too.
pub(crate) fn window_font_css(
    desc: &str,
    scale: f64,
    chrome_size: &str,
    chrome_scale: f64,
) -> String {
    let size = effective_font_size_css(desc, scale);
    let chrome = effective_font_size_css(chrome_size, chrome_scale);
    format!(
        "window.forge-window,\n\
         window.forge-window label:not(.tab-conn-dot),\n\
         window.forge-window entry,\n\
         window.forge-window textview,\n\
         window.forge-window text,\n\
         window.forge-window treeview {{ font-size: {size}; }}\n\
         window.forge-window .forge-font-chrome,\n\
         window.forge-window .forge-font-chrome label:not(.tab-conn-dot),\n\
         window.forge-window .forge-font-chrome entry,\n\
         window.forge-window .forge-font-chrome textview,\n\
         window.forge-window .forge-font-chrome text,\n\
         window.forge-window .forge-font-chrome treeview {{ font-size: {chrome}; }}"
    )
}

/// Native popup text follows the configured font without inheriting a
/// selected tab's bold/italic decoration. Pango parses size/style boundaries;
/// the existing CSS stack encoder quotes family names safely.
pub(crate) fn menu_font_css(desc: &str) -> String {
    let font = font_description(desc, None);
    let family = font.family().unwrap_or_else(|| "sans-serif".into());
    let stack = family
        .split(',')
        .map(str::trim)
        .map(|family| css_font_stack(family, None))
        .collect::<Vec<_>>()
        .join(", ");
    let mut css = format!("font-family: {stack}; font-weight: normal; font-style: normal;");
    if font.size() > 0 {
        let size = f64::from(font.size()) / f64::from(gtk::pango::SCALE);
        let unit = if font.is_size_absolute() { "px" } else { "pt" };
        css.push_str(&format!(" font-size: {size}{unit};"));
    }
    css
}

/// Theme only opted-in popup surfaces; native menus elsewhere retain their
/// own presentation. The display-wide provider updates an already-open menu.
pub(crate) fn menu_theme_css(
    desc: &str,
    background: &gtk::gdk::RGBA,
    foreground: &gtk::gdk::RGBA,
) -> String {
    let color = |color: &gtk::gdk::RGBA| {
        format!(
            "rgb({},{},{})",
            (color.red() * 255.0) as u8,
            (color.green() * 255.0) as u8,
            (color.blue() * 255.0) as u8
        )
    };
    let bg = color(background);
    let fg = color(foreground);
    let font = menu_font_css(desc);
    format!(
        r#"
        .terminal-context-menu, .terminal-context-menu label,
        .terminal-context-menu button, .terminal-context-menu entry {{ {font} }}
        .terminal-context-menu > contents {{ background-color: {bg}; color: {fg}; border: 1px solid alpha({fg},0.22); }}
        .terminal-context-menu > arrow {{ background-color: {bg}; border-color: alpha({fg},0.22); }}
        .terminal-context-menu button {{ color: {fg}; background-color: transparent; background-image: none; box-shadow: none; border-color: transparent; }}
        .terminal-context-menu button:hover, .terminal-context-menu button:active,
        .terminal-context-menu button:focus-visible {{ outline-color: alpha({fg},0.5); background-color: alpha({fg},0.12); }}
        .terminal-context-menu button:disabled {{ color: alpha({fg},0.45); }}
        .terminal-context-menu separator {{ background-color: alpha({fg},0.18); }}
        .terminal-context-menu scrolledwindow, .terminal-context-menu viewport,
        .terminal-context-menu scrollbar, .terminal-context-menu scrollbar trough {{ background-color: transparent; }}
        .terminal-context-menu scrollbar slider {{ background-color: alpha({fg},0.3); }}
        .terminal-context-menu scrollbar slider:hover {{ background-color: alpha({fg},0.5); }}
        .terminal-context-menu entry {{ color: {fg}; background-color: alpha({fg},0.06); caret-color: {fg}; }}
    "#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires a real GTK/Pango display context"]
    fn reference_content_absolute_size_uses_config_default_and_native_context() {
        use gtk::pango::prelude::FontExt;

        gtk::init().unwrap();
        let window = gtk::Window::new();
        window.present();
        let context = window.pango_context();
        let config = Config::safe_defaults();
        let nominal = FontDescription::from_string(&config.font_desc);
        assert_eq!(nominal.size(), 14 * gtk::pango::SCALE);
        assert!(!nominal.is_size_absolute());
        let nominal_px = f64::from(
            context
                .load_font(&nominal)
                .unwrap()
                .describe_with_absolute_size()
                .size(),
        ) / f64::from(gtk::pango::SCALE);
        assert_eq!(
            reference_content_role_multiplier(&config.font_desc, 1.0, &context),
            1.0
        );
        for (size, absolute, scale) in [
            (14.0, false, 1.0),
            (28.0, false, 1.0),
            (13.5, false, 1.3),
            (14.0, true, 1.0),
            (28.0, true, 1.0),
            (18.25, true, 1.3),
        ] {
            let unit = if absolute { "px" } else { "" };
            let factor = reference_content_role_multiplier(
                &format!("Monospace {size}{unit}"),
                scale,
                &context,
            );
            let expected = if absolute {
                size / nominal_px
            } else {
                size / 14.0
            } * scale;
            assert!((factor - expected).abs() < 0.001, "{factor} vs {expected}");
            eprintln!("reference-content-native: size={size}{unit} scale={scale} nominal_px={nominal_px} factor={factor} widget_scale={}", window.scale_factor());
        }
        window.close();
    }

    #[test]
    #[ignore = "requires a real GTK/Pango display context"]
    fn reference_ui_absolute_size_uses_native_context_without_dpr_multiplier() {
        use gtk::pango::prelude::FontExt;

        gtk::init().unwrap();
        let window = gtk::Window::new();
        window.present();
        let context = window.pango_context();
        let mut nominal = context.font_description().unwrap();
        nominal.set_size(14 * gtk::pango::SCALE);
        let nominal_px = f64::from(
            context
                .load_font(&nominal)
                .unwrap()
                .describe_with_absolute_size()
                .size(),
        ) / f64::from(gtk::pango::SCALE);
        assert_eq!(reference_ui_role_multiplier("14", 1.0, &context), 1.0);
        for (size, absolute, scale) in [
            (14.0, false, 1.0),
            (28.0, false, 1.0),
            (13.5, false, 1.3),
            (14.0, true, 1.0),
            (28.0, true, 1.0),
            (18.25, true, 1.3),
        ] {
            let unit = if absolute { "px" } else { "" };
            let factor = reference_ui_role_multiplier(&format!("{size}{unit}"), scale, &context);
            let expected = if absolute {
                size / nominal_px
            } else {
                size / 14.0
            } * scale;
            assert!((factor - expected).abs() < 0.001, "{factor} vs {expected}");
            eprintln!("reference-ui-native: size={size}{unit} scale={scale} nominal_px={nominal_px} factor={factor} widget_scale={}", window.scale_factor());
        }
        // These are the unchanged loader's frozen legacy point/pixel strings.
        // Editing Main Content afterward cannot alter this helper's inputs.
        for legacy in ["Monospace 14", "Monospace 16px"] {
            let mut config = Config::safe_defaults();
            config.font_desc = legacy.into();
            config.chrome_font_size = unscaled_font_size(legacy);
            config.chrome_font_scale = crate::font_zoom::initial_chrome_scale(None, 1.5);
            let before = reference_ui_role_multiplier(
                &config.chrome_font_size,
                config.chrome_font_scale,
                &context,
            );
            config.font_desc = "Monospace 32".into();
            config.default_font_scale = 1.8;
            assert_eq!(
                reference_ui_role_multiplier(
                    &config.chrome_font_size,
                    config.chrome_font_scale,
                    &context
                ),
                before
            );
        }
        window.close();
    }

    #[test]
    fn default_fallback_is_narrow_and_keeps_the_saved_default() {
        let configured = FontDescription::from_string(crate::config::DEFAULT_FONT_DESC);
        assert_eq!(configured.family().as_deref(), Some(DEFAULT_FONT_FAMILY));
        assert_eq!(
            default_family_fallback(DEFAULT_FONT_FAMILY, false),
            "Monospace"
        );
        assert_eq!(
            default_family_fallback(DEFAULT_FONT_FAMILY, true),
            DEFAULT_FONT_FAMILY
        );
        assert_eq!(
            default_family_fallback("jetbrainsmono nerd font mono", false),
            "Monospace"
        );
        for custom in [
            "",
            "Monospace",
            "User Font",
            "JetBrainsMono Nerd Font",
            "JetBrainsMono Nerd Font Mono, Monospace",
        ] {
            assert_eq!(default_family_fallback(custom, false), custom);
        }
    }

    #[test]
    #[ignore = "requires a GTK display"]
    fn default_font_resolution_preserves_grid_and_configuration() {
        use vte4::TerminalExt;
        // A pre-GTK lookup must not poison the runtime availability cache.
        let _ = font_description(crate::config::DEFAULT_FONT_DESC, None);
        gtk4::init().expect("GTK display");
        let config = Config::safe_defaults();
        let before = config.font_desc.clone();
        let context = gtk4::Label::new(None).pango_context();
        let installed = context
            .list_families()
            .iter()
            .any(|family| family.name().eq_ignore_ascii_case(DEFAULT_FONT_FAMILY));
        let expected = default_family_fallback(DEFAULT_FONT_FAMILY, installed);
        let actual = font_description("JetBrainsMono Nerd Font Mono Bold Italic 13.5", None);
        assert_eq!(actual.family().as_deref(), Some(expected));
        assert_eq!(actual.size(), (13.5 * gtk4::pango::SCALE as f64) as i32);
        assert_eq!(actual.weight(), gtk4::pango::Weight::Bold);
        assert_eq!(actual.style(), gtk4::pango::Style::Italic);
        assert_eq!(
            css_font_stack(DEFAULT_FONT_FAMILY, None),
            format!("\"{expected}\"")
        );
        let terminal = crate::terminal::create_terminal(&config);
        let reference = vte4::Terminal::new();
        reference.set_font(Some(&font_description(
            &format!("{expected} 14"),
            icon_family(&config),
        )));
        let row = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
        for surface in [&terminal, &reference] {
            surface.set_size(32, 6);
            surface.set_hexpand(true);
            surface.set_vexpand(true);
            row.append(surface);
        }
        let window = gtk4::Window::builder()
            .default_width(1000)
            .default_height(240)
            .child(&row)
            .build();
        window.present();
        let pump = || {
            for _ in 0..20 {
                for _ in 0..64 {
                    if !gtk4::glib::MainContext::default().iteration(false) {
                        break;
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        };
        pump();
        for surface in [&terminal, &reference] {
            surface.feed("iiii WWWW\r\n你好 😀 e\u{301}\r\nA中😀e\u{301}".as_bytes());
        }
        pump();
        assert_eq!(terminal.char_width(), reference.char_width());
        assert_eq!(terminal.char_height(), reference.char_height());
        assert_eq!(terminal.cursor_position(), (6, 2));
        assert_eq!(terminal.cursor_position(), reference.cursor_position());
        assert_eq!(
            terminal.text_format(vte4::Format::Text),
            reference.text_format(vte4::Format::Text)
        );
        window.close();
        assert_eq!(config.font_desc, before);
        assert_eq!(before, crate::config::DEFAULT_FONT_DESC);
    }

    #[test]
    fn mono_cuts_outrank_proportional_and_symbols_outrank_text_fonts() {
        assert!(
            icon_family_rank("Symbols Nerd Font Mono") < icon_family_rank("Hack Nerd Font Mono")
        );
        assert!(icon_family_rank("Hack Nerd Font Mono") < icon_family_rank("Symbols Nerd Font"));
        assert!(icon_family_rank("Hack Nerd Font") < icon_family_rank("Hack Nerd Font Propo"));
        assert_eq!(icon_family_rank("DejaVu Sans Mono"), None);
        assert_eq!(icon_family_rank("AR PL UMing HK"), None);
    }

    #[test]
    fn selection_prefers_the_mono_cut_and_breaks_ties_by_name() {
        let installed = [
            "AR PL UMing HK",
            "JetBrainsMono Nerd Font Propo",
            "SauceCodePro Nerd Font",
            "JetBrainsMono Nerd Font Mono",
            "DejaVuSansM Nerd Font Mono",
        ]
        .map(str::to_string);
        assert_eq!(
            select_icon_family(installed),
            Some("DejaVuSansM Nerd Font Mono".to_string())
        );
    }

    #[test]
    fn selection_is_none_without_an_icon_font() {
        let installed = ["DejaVu Sans Mono", "Arial"].map(str::to_string);
        assert_eq!(select_icon_family(installed), None);
    }

    #[test]
    fn a_font_that_already_draws_icons_is_left_alone() {
        assert_eq!(
            family_list_with_icon("JetBrainsMono Nerd Font", "Symbols Nerd Font Mono"),
            None
        );
        assert_eq!(family_list_with_icon("", "Symbols Nerd Font Mono"), None);
        assert_eq!(
            family_list_with_icon("Symbols Nerd Font Mono", "Symbols Nerd Font Mono"),
            None
        );
    }

    #[test]
    fn the_icon_family_lands_behind_the_configured_one() {
        assert_eq!(
            family_list_with_icon("Monospace", "Symbols Nerd Font Mono"),
            Some("Monospace,Symbols Nerd Font Mono".to_string())
        );
    }

    #[test]
    fn size_and_style_survive_the_rewrite() {
        let font = font_description("Monospace Bold Italic 14", Some("Symbols Nerd Font Mono"));
        assert_eq!(
            font.family().map(|f| f.to_string()).unwrap_or_default(),
            "Monospace,Symbols Nerd Font Mono"
        );
        assert_eq!(font.size() / gtk4::pango::SCALE, 14);
        assert_eq!(font.weight(), gtk4::pango::Weight::Bold);
        assert_eq!(font.style(), gtk4::pango::Style::Italic);
    }

    #[test]
    fn no_icon_family_leaves_the_description_untouched() {
        let font = font_description("Monospace 14", None);
        assert_eq!(
            font.family().map(|f| f.to_string()).unwrap_or_default(),
            "Monospace"
        );
    }

    /// The wiring, not just the arithmetic: a live VTE built from a config
    /// whose font covers no icons must be handed the fallback family too.
    /// Needs a display, so it runs from `make test-display`.
    #[test]
    #[ignore = "requires a GTK display"]
    fn a_real_vte_is_given_the_icon_family_behind_the_configured_one() {
        use vte4::TerminalExt;

        gtk4::init().expect("GTK display");
        let mut config = Config::safe_defaults();
        config.font_desc = "Monospace 14".to_string();
        let configured = FontDescription::from_string(&config.font_desc)
            .family()
            .map(|family| family.to_string())
            .expect("the default font names a family");
        let terminal = crate::terminal::create_terminal(&config);
        let families = terminal
            .font()
            .and_then(|font| font.family())
            .map(|family| family.to_string())
            .expect("the terminal carries a font");

        match icon_family(&config) {
            Some(icon) => assert_eq!(families, format!("{configured},{icon}")),
            // A box with no Nerd Font installed has nothing to fall back to,
            // and the description must then be exactly what the user asked for.
            None => assert_eq!(families, configured),
        }
    }

    #[test]
    #[ignore = "requires a GTK display"]
    fn an_explicit_appended_fallback_resolves_on_a_real_vte() {
        use vte4::TerminalExt;
        gtk4::init().expect("GTK display");
        let mut config = Config::safe_defaults();
        config.font_desc = "Forge QA Unavailable Family 14".to_string();
        config.icon_font = Some("Monospace".to_string());
        let before = config.font_desc.clone();
        let terminal = crate::terminal::create_terminal(&config);
        let context = terminal.pango_context();
        assert!(!context
            .list_families()
            .iter()
            .any(|family| family.name() == "Forge QA Unavailable Family"));
        let reference_description = FontDescription::from_string("Monospace 14");
        let actual_description = terminal.font().expect("configured font");
        let actual_family = context
            .load_font(&actual_description)
            .unwrap()
            .describe()
            .family()
            .unwrap();
        let expected_family = context
            .load_font(&reference_description)
            .unwrap()
            .describe()
            .family()
            .unwrap();
        assert_eq!(
            actual_family, expected_family,
            "the appended family must actually resolve"
        );

        let reference = vte4::Terminal::new();
        reference.set_font(Some(&reference_description));
        let row = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
        for surface in [&terminal, &reference] {
            surface.set_size(32, 6);
            surface.set_hexpand(true);
            surface.set_vexpand(true);
            row.append(surface);
        }
        let window = gtk4::Window::builder()
            .default_width(1000)
            .default_height(240)
            .child(&row)
            .build();
        window.present();
        let pump = || {
            for _ in 0..20 {
                for _ in 0..64 {
                    if !gtk4::glib::MainContext::default().iteration(false) {
                        break;
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        };
        pump();
        for surface in [&terminal, &reference] {
            surface.feed("iiii WWWW\r\n你好 😀 e\u{301}\r\nA中😀e\u{301}".as_bytes());
        }
        pump();
        assert_eq!(terminal.char_width(), reference.char_width());
        assert_eq!(terminal.char_height(), reference.char_height());
        assert_eq!(terminal.cursor_position(), (6, 2));
        assert_eq!(terminal.cursor_position(), reference.cursor_position());
        assert_eq!(
            terminal.text_format(vte4::Format::Text),
            reference.text_format(vte4::Format::Text)
        );
        window.close();
        assert_eq!(config.font_desc, before);
        assert_eq!(config.icon_font.as_deref(), Some("Monospace"));
    }

    #[test]
    #[ignore = "requires DISPLAY"]
    fn context_menu_tracks_live_theme_and_font_without_parent_emphasis() {
        gtk::init().expect("GTK display");
        struct AnimationPreference(gtk::Settings, bool);
        impl Drop for AnimationPreference {
            fn drop(&mut self) {
                self.0.set_gtk_enable_animations(self.1);
            }
        }
        let settings = gtk::Settings::default().unwrap();
        let _animations =
            AnimationPreference(settings.clone(), settings.is_gtk_enable_animations());
        settings.set_gtk_enable_animations(false);
        let provider = gtk::CssProvider::new();
        let errors = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let caught = errors.clone();
        provider
            .connect_parsing_error(move |_, _, error| caught.borrow_mut().push(error.to_string()));
        let display = gtk::gdk::Display::default().unwrap();
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1,
        );
        let window = gtk::Window::new();
        // Leave room for the parent tab when its emphasis style is applied.
        window.set_default_size(640, 480);
        let tab = gtk::ToggleButton::with_label("Selected tab");
        tab.add_css_class("tab-strip-btn");
        tab.set_active(true);
        window.set_child(Some(&tab));
        let popover = gtk::Popover::new();
        popover.add_css_class("terminal-context-menu");
        popover.set_has_arrow(false);
        popover.set_parent(&tab);
        let menu = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let item = gtk::Button::with_label("Rename tab");
        item.set_has_frame(false);
        item.add_css_class("flat");
        menu.append(&item);
        let scroll = crate::context_menu::scrolled_content(&menu, &tab);
        popover.set_child(Some(&scroll));
        window.present();
        popover.popup();
        let main = gtk::glib::MainContext::default();
        for (name, bg, fg, desc, size) in [
            (
                "dark",
                "#16161e",
                "#c0caf5",
                "Monospace Bold Italic 18px",
                18,
            ),
            ("light", "#f6f8fa", "#24292f", "Sans Italic 22px", 22),
            ("custom", "#221122", "#ffddee", "Monospace Bold 16px", 16),
        ] {
            let bg = gtk::gdk::RGBA::parse(bg).unwrap();
            let fg = gtk::gdk::RGBA::parse(fg).unwrap();
            let css = format!(
                ".tab-strip-btn {{ font-weight: bold; font-style: italic; }} {}",
                menu_theme_css(desc, &bg, &fg)
            );
            provider.load_from_string(&css);
            let deadline = std::time::Instant::now() + std::time::Duration::from_millis(200);
            while std::time::Instant::now() < deadline {
                while main.iteration(false) {}
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            let label = item.child().unwrap().downcast::<gtk::Label>().unwrap();
            let actual = label.pango_context().font_description().unwrap();
            assert_eq!(
                actual.family(),
                FontDescription::from_string(desc).family(),
                "{name}"
            );
            assert_eq!(actual.weight(), gtk::pango::Weight::Normal, "{name}");
            assert_eq!(actual.style(), gtk::pango::Style::Normal, "{name}");
            assert_eq!(actual.size(), size * gtk::pango::SCALE, "{name}");
            let actual_fg = label.color();
            for (actual, expected) in [
                (actual_fg.red(), fg.red()),
                (actual_fg.green(), fg.green()),
                (actual_fg.blue(), fg.blue()),
            ] {
                assert!((actual - expected).abs() < 0.01, "{name}");
            }
            let snapshot = gtk::Snapshot::new();
            gtk::WidgetPaintable::new(Some(&popover)).snapshot(
                &snapshot,
                f64::from(popover.width()),
                f64::from(popover.height()),
            );
            let node = snapshot.to_node().expect("mapped popup snapshot");
            let renderer = gtk::gsk::Renderer::for_surface(&window.surface().unwrap()).unwrap();
            let texture = renderer.render_texture(&node, None);
            let stride = texture.width() as usize * 4;
            let mut pixels = vec![0; stride * texture.height() as usize];
            texture.download(&mut pixels, stride);
            let rgb = [
                (bg.red() * 255.0) as u8,
                (bg.green() * 255.0) as u8,
                (bg.blue() * 255.0) as u8,
            ];
            let matching = pixels
                .as_chunks::<4>()
                .0
                .iter()
                .filter(|pixel| {
                    let argb = u32::from_ne_bytes([pixel[0], pixel[1], pixel[2], pixel[3]]);
                    [
                        ((argb >> 16) & 255) as u8,
                        ((argb >> 8) & 255) as u8,
                        (argb & 255) as u8,
                    ] == rgb
                })
                .count();
            assert!(
                matching > 100,
                "{name}: popup background must use the configured color"
            );
            if let Some(directory) = std::env::var_os("JTERM_MENU_SCREENSHOTS") {
                std::fs::create_dir_all(&directory).unwrap();
                let path = std::path::Path::new(&directory)
                    .join(format!("{}-{name}.png", env!("CARGO_PKG_NAME")));
                texture.save_to_png(path).unwrap();
            }
            renderer.unrealize();
        }
        assert!(
            scroll.height() < scroll.max_content_height(),
            "small menus keep their natural height"
        );
        let mut last = item.clone();
        for index in 1..32 {
            last = gtk::Button::with_label(&format!(
                "Remote host {index}: a complete long label remains accessible"
            ));
            last.set_has_frame(false);
            last.add_css_class("flat");
            menu.append(&last);
        }
        provider.load_from_string(&menu_theme_css(
            "Monospace 48",
            &gtk::gdk::RGBA::BLACK,
            &gtk::gdk::RGBA::WHITE,
        ));
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(200);
        while std::time::Instant::now() < deadline {
            while main.iteration(false) {}
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let vertical = scroll.vadjustment();
        assert!(
            vertical.upper() > vertical.page_size(),
            "large menus must scroll"
        );
        assert!(
            scroll.height() <= scroll.max_content_height() + 24,
            "viewport remains monitor-bounded including scrollbar chrome"
        );
        vertical.set_value(vertical.upper() - vertical.page_size());
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(100);
        while std::time::Instant::now() < deadline {
            while main.iteration(false) {}
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let last_bounds = last.compute_bounds(&scroll).unwrap();
        assert!(
            last_bounds.y() >= 0.0
                && last_bounds.y() + last_bounds.height() <= scroll.height() as f32 + 1.0,
            "last item is reachable by scrolling: {last_bounds:?}"
        );
        let horizontal = scroll.hadjustment();
        assert!(
            horizontal.upper() > horizontal.page_size(),
            "long labels scroll horizontally instead of being truncated"
        );
        assert!(errors.borrow().is_empty(), "{:?}", errors.borrow());
        popover.popdown();
        popover.unparent();
        window.close();
        gtk::style_context_remove_provider_for_display(&display, &provider);
    }

    #[test]
    fn chrome_size_preserves_pango_units_and_fractional_zoom() {
        for (desc, scale, expected) in [
            ("Monospace 14", 1.0, "14pt"),
            ("Monospace Bold Italic 13.5", 1.025, "13.837499999999999pt"),
            ("Monospace 17px", 1.25, "21.25px"),
            ("Monospace, Serif 12.5", 0.9, "11.25pt"),
            ("Monospace 14", 0.1, "1.4000000000000001pt"),
        ] {
            assert_eq!(effective_font_size_css(desc, scale), expected);
        }
        for scale in [f64::NAN, f64::INFINITY, 0.0, -1.0] {
            assert_eq!(effective_font_size_css("Monospace 14", scale), "14pt");
        }
        assert_eq!(effective_font_size_css("Monospace", 1.0), "14pt");
    }

    #[test]
    fn chrome_size_is_scoped_to_text_and_preserves_icon_geometry() {
        let css = window_font_css("Monospace Bold 12.5", 1.2, "12.5", 0.8);
        assert!(css.contains("font-size: 15pt;"));
        assert!(css.contains("font-size: 10pt;"));
        assert!(css.contains(".forge-font-chrome label:not(.tab-conn-dot)"));
        assert!(css.contains("label:not(.tab-conn-dot)"));
        assert!(css.contains("window.forge-window text,"));
        assert!(css.contains("entry"));
        assert!(!css.contains("image"));
        assert!(!css.contains("font-family"));
        assert!(!css.contains("font-weight"));
        for selector in css.split('{').next().unwrap().split(',') {
            assert!(selector.trim().starts_with("window.forge-window"));
        }
    }

    #[test]
    fn size_only_description_retains_units_and_fractional_points() {
        assert_eq!(unscaled_font_size("Monospace Bold 13.5"), "13.5");
        assert_eq!(unscaled_font_size("Monospace 17px"), "17px");
        assert_eq!(effective_font_size_css("13.5", 1.0), "13.5pt");
        assert_eq!(effective_font_size_css("17px", 1.0), "17px");
    }

    #[test]
    fn settings_font_size_changes_do_not_cross_group_boundaries() {
        let initial = window_font_css("Monospace 14", 1.25, "13.5", 0.8);
        let main_changed = window_font_css("Monospace 28", 1.25, "13.5", 0.8);
        let marker = "window.forge-window .forge-font-chrome,";
        assert_eq!(
            initial.split(marker).nth(1),
            main_changed.split(marker).nth(1)
        );
        assert_ne!(
            initial.split(marker).next(),
            main_changed.split(marker).next()
        );
        let chrome_changed = window_font_css("Monospace 14", 1.25, "17px", 0.8);
        assert_eq!(
            initial.split(marker).next(),
            chrome_changed.split(marker).next()
        );
        assert_ne!(
            initial.split(marker).nth(1),
            chrome_changed.split(marker).nth(1)
        );
    }

    #[test]
    fn popup_font_uses_pango_family_and_fractional_size_without_tab_emphasis() {
        let css = menu_font_css("Monospace Bold Italic 13.5");
        assert!(css.contains("font-family: \"Monospace\";"));
        assert!(css.contains("font-size: 13.5pt;"));
        assert!(css.contains("font-weight: normal;"));
        assert!(css.contains("font-style: normal;"));
        assert!(menu_font_css("Monospace 17px").contains("font-size: 17px;"));
        assert!(
            menu_font_css("Monospace, Serif 12").contains("font-family: \"Monospace\", \"Serif\";")
        );
    }

    #[test]
    fn the_css_stack_quotes_and_escapes_both_families() {
        assert_eq!(
            css_font_stack("Monospace", Some("Symbols Nerd Font Mono")),
            "\"Monospace\", \"Symbols Nerd Font Mono\""
        );
        assert_eq!(css_font_stack("Monospace", None), "\"Monospace\"");
        assert_eq!(
            css_font_stack("Hack Nerd Font", Some("Symbols Nerd Font Mono")),
            "\"Hack Nerd Font\""
        );
        assert_eq!(
            css_font_stack("ev\"il", Some("Symbols Nerd Font Mono")),
            "\"ev\\\"il\", \"Symbols Nerd Font Mono\""
        );
    }
    #[test]
    fn settings_edits_preserve_units_fractional_sizes_and_style() {
        let pixel = font_description_with_size("Monospace Bold Italic 17px", 18.25);
        assert_eq!(effective_font_size_css(&pixel, 1.0), "18.25px");
        assert!(pixel.contains("Bold"));
        assert!(pixel.contains("Italic"));
        let points = font_description_with_family("Monospace Bold Italic 13.5", "Serif");
        assert_eq!(effective_font_size_css(&points, 1.0), "13.5pt");
        assert!(points.contains("Bold"));
        assert!(points.contains("Italic"));
        assert!(points.contains("Serif"));
        let pixels = font_description_with_family(&pixel, "Serif");
        assert_eq!(effective_font_size_css(&pixels, 1.0), "18.25px");
        let css = window_font_css(&pixels, 1.0, "13.5", 1.0);
        assert!(css.contains("font-size: 18.25px"));
        assert!(css.contains("font-size: 13.5pt"));
        assert_eq!(
            effective_font_size_css(&font_description_with_size(&points, f64::NAN), 1.0),
            "13.5pt"
        );
    }
}
