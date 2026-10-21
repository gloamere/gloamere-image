//! Shared surfaces and metrics; changing appearance never touches image resources.
use eframe::egui::{self, Color32, Stroke, Vec2};
use light_image_tool::settings::Appearance;

#[derive(Clone, Copy, Debug)]
pub(crate) struct Palette {
    pub window: Color32,
    pub canvas: Color32,
    pub panel: Color32,
    pub toolbar: Color32,
    pub popover: Color32,
    pub input: Color32,
    pub text: Color32,
    pub muted: Color32,
    pub icon: Color32,
    pub disabled: Color32,
    pub border: Color32,
    pub separator: Color32,
    pub hover: Color32,
    pub pressed: Color32,
    pub selected: Color32,
    pub accent: Color32,
    pub error: Color32,
    pub error_bg: Color32,
    pub warning: Color32,
}

impl Palette {
    pub(crate) fn for_theme(theme: egui::Theme) -> Self {
        let rgb = |hex: u32| Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8);
        match theme {
            egui::Theme::Light => Self {
                window: rgb(0xF5F6F7),
                canvas: rgb(0xECECEC),
                panel: rgb(0xFAFBFC),
                toolbar: Color32::WHITE,
                popover: Color32::WHITE,
                input: Color32::WHITE,
                text: rgb(0x252C32),
                muted: rgb(0x626B73),
                icon: rgb(0x3D4750),
                disabled: rgb(0x8B949B),
                border: rgb(0xDDE1E4),
                separator: rgb(0xE4E7E9),
                hover: rgb(0xE9EDF0),
                pressed: rgb(0xDEE6EB),
                selected: rgb(0xE8EEF1),
                accent: rgb(0x416A7D),
                error: rgb(0xA33232),
                error_bg: rgb(0xFFF0F0),
                warning: rgb(0x875510),
            },
            egui::Theme::Dark => Self {
                window: rgb(0x222426),
                canvas: rgb(0x2C2C2C),
                panel: rgb(0x26292C),
                toolbar: rgb(0x32363A),
                popover: rgb(0x32363A),
                input: rgb(0x23272A),
                text: rgb(0xEBEEF0),
                muted: rgb(0xA9B2B9),
                icon: rgb(0xD0D7DC),
                disabled: rgb(0x737D85),
                border: rgb(0x454C52),
                separator: rgb(0x3B4146),
                hover: rgb(0x3B4146),
                pressed: rgb(0x46515A),
                selected: rgb(0x34454F),
                accent: rgb(0xA1C6D5),
                error: rgb(0xFFAAAA),
                error_bg: rgb(0x492E32),
                warning: rgb(0xF3CB81),
            },
        }
    }
}

pub(crate) fn palette(ctx: &egui::Context) -> Palette {
    Palette::for_theme(ctx.theme())
}

pub(crate) fn preference(appearance: Appearance) -> egui::ThemePreference {
    match appearance {
        Appearance::System => egui::ThemePreference::System,
        Appearance::Light => egui::ThemePreference::Light,
        Appearance::Dark => egui::ThemePreference::Dark,
    }
}

pub(crate) fn configure(ctx: &egui::Context) {
    for theme in [egui::Theme::Light, egui::Theme::Dark] {
        let p = Palette::for_theme(theme);
        ctx.style_mut_of(theme, |style| {
            style.spacing.item_spacing = Vec2::new(8.0, 6.0);
            style.spacing.button_padding = Vec2::new(8.0, 5.0);
            style.spacing.interact_size.y = 32.0;
            style.spacing.menu_margin = egui::Margin::same(6);
            for (kind, size) in [
                (egui::TextStyle::Small, 12.0),
                (egui::TextStyle::Body, 13.0),
                (egui::TextStyle::Button, 13.0),
                (egui::TextStyle::Monospace, 12.0),
                (egui::TextStyle::Heading, 16.0),
            ] {
                let family = if kind == egui::TextStyle::Monospace {
                    egui::FontFamily::Monospace
                } else {
                    egui::FontFamily::Proportional
                };
                style
                    .text_styles
                    .insert(kind, egui::FontId::new(size, family));
            }
            let v = &mut style.visuals;
            v.dark_mode = theme == egui::Theme::Dark;
            v.weak_text_color = Some(p.muted);
            v.panel_fill = p.panel;
            v.window_fill = p.popover;
            v.window_stroke = Stroke::new(1.0, p.border);
            v.window_corner_radius = egui::CornerRadius::same(8);
            v.menu_corner_radius = egui::CornerRadius::same(8);
            v.window_shadow = egui::epaint::Shadow::NONE;
            v.popup_shadow = egui::epaint::Shadow::NONE;
            v.extreme_bg_color = p.input;
            v.text_edit_bg_color = Some(p.input);
            v.faint_bg_color = p.hover;
            v.code_bg_color = p.selected;
            v.selection.bg_fill = p.selected;
            v.selection.stroke = Stroke::new(1.0, p.accent);
            v.hyperlink_color = p.accent;
            v.warn_fg_color = p.warning;
            v.error_fg_color = p.error;
            v.text_cursor.stroke.color = p.accent;
            for (widget, fill, weak, stroke, foreground) in [
                (
                    &mut v.widgets.noninteractive,
                    p.popover,
                    p.popover,
                    p.separator,
                    p.text,
                ),
                (
                    &mut v.widgets.inactive,
                    p.input,
                    Color32::TRANSPARENT,
                    p.border,
                    p.icon,
                ),
                (&mut v.widgets.hovered, p.hover, p.hover, p.border, p.text),
                (
                    &mut v.widgets.active,
                    p.pressed,
                    p.pressed,
                    p.accent,
                    p.text,
                ),
                (
                    &mut v.widgets.open,
                    p.selected,
                    p.selected,
                    p.border,
                    p.accent,
                ),
            ] {
                widget.bg_fill = fill;
                widget.weak_bg_fill = weak;
                widget.bg_stroke = Stroke::new(1.0, stroke);
                widget.fg_stroke = Stroke::new(1.5, foreground);
                widget.corner_radius = egui::CornerRadius::same(5);
                widget.expansion = 0.0;
            }
        });
    }
    ctx.options_mut(|options| options.sync_window_theme = true);
    ctx.set_theme(egui::ThemePreference::System);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn themes_share_geometry_and_canvas_is_neutral() {
        let ctx = egui::Context::default();
        configure(&ctx);
        let light = ctx.style_of(egui::Theme::Light);
        let dark = ctx.style_of(egui::Theme::Dark);
        assert_eq!(light.spacing, dark.spacing);
        assert_eq!(light.text_styles, dark.text_styles);
        for theme in [egui::Theme::Light, egui::Theme::Dark] {
            let color = Palette::for_theme(theme).canvas;
            assert_eq!(color.r(), color.g());
            assert_eq!(color.g(), color.b());
        }
    }

    #[test]
    fn system_theme_updates_and_manual_preference_wins() {
        let ctx = egui::Context::default();
        configure(&ctx);
        for theme in [egui::Theme::Light, egui::Theme::Dark] {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    system_theme: Some(theme),
                    ..Default::default()
                },
                |_| {},
            );
            output.textures_delta.clear();
            assert_eq!(ctx.theme(), theme);
            assert_eq!(palette(&ctx).canvas, Palette::for_theme(theme).canvas);
        }
        ctx.set_theme(preference(Appearance::Light));
        let mut output = ctx.run_ui(
            egui::RawInput {
                system_theme: Some(egui::Theme::Dark),
                ..Default::default()
            },
            |_| {},
        );
        output.textures_delta.clear();
        assert_eq!(ctx.theme(), egui::Theme::Light);
    }
}
