//! Тема оформления: выбор и палитры egui.

use eframe::egui;
use serde::{Deserialize, Serialize};

/// Тема оформления.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize, Default)]
pub(crate) enum ThemeChoice {
    #[default]
    System,
    Dark,
    Light,
    Nord,
}

impl ThemeChoice {
    pub(crate) fn next(self) -> Self {
        match self {
            ThemeChoice::System => ThemeChoice::Dark,
            ThemeChoice::Dark => ThemeChoice::Light,
            ThemeChoice::Light => ThemeChoice::Nord,
            ThemeChoice::Nord => ThemeChoice::System,
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            ThemeChoice::System => "System",
            ThemeChoice::Dark => "Dark",
            ThemeChoice::Light => "Light",
            ThemeChoice::Nord => "Nord",
        }
    }
}

/// Nord — «арктическая» тёмная палитра (nordtheme.com): Polar Night для фонов,
/// Snow Storm для текста, Frost для акцентов, Aurora для статусов.
fn nord_visuals() -> egui::Visuals {
    use egui::{Color32, Stroke};

    const NORD0: Color32 = Color32::from_rgb(0x2e, 0x34, 0x40); // Polar Night: фон
    const NORD1: Color32 = Color32::from_rgb(0x3b, 0x42, 0x52); // панели/виджеты
    const NORD2: Color32 = Color32::from_rgb(0x43, 0x4c, 0x5e); // выделение/ховер
    const NORD3: Color32 = Color32::from_rgb(0x4c, 0x56, 0x6a); // границы
    const NORD4: Color32 = Color32::from_rgb(0xd8, 0xde, 0xe9); // Snow Storm: текст
    const NORD6: Color32 = Color32::from_rgb(0xec, 0xef, 0xf4); // яркий текст
    const NORD8: Color32 = Color32::from_rgb(0x88, 0xc0, 0xd0); // Frost: основной акцент
    const NORD9: Color32 = Color32::from_rgb(0x81, 0xa1, 0xc1); // вторичный акцент
    const NORD10: Color32 = Color32::from_rgb(0x5e, 0x81, 0xac); // третичный

    let mut v = egui::Visuals::dark();
    v.override_text_color = Some(NORD4);
    v.panel_fill = NORD0;
    v.window_fill = NORD1;
    v.faint_bg_color = NORD1;
    v.extreme_bg_color = NORD0;
    v.code_bg_color = NORD1;
    v.hyperlink_color = NORD8;
    v.selection.bg_fill = Color32::from_rgba_unmultiplied(NORD10.r(), NORD10.g(), NORD10.b(), 110);
    v.selection.stroke = Stroke::new(1.0, NORD9);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, NORD4);
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, NORD2);
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, NORD4);
    v.widgets.inactive.bg_fill = NORD1;
    v.widgets.inactive.weak_bg_fill = NORD1;
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, NORD2);
    v.widgets.hovered.fg_stroke = Stroke::new(1.5, NORD6);
    v.widgets.hovered.bg_fill = NORD2;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, NORD3);
    v.widgets.active.fg_stroke = Stroke::new(2.0, NORD6);
    v.widgets.active.bg_fill = NORD2;
    v.widgets.active.bg_stroke = Stroke::new(1.0, NORD3);
    v.widgets.open.fg_stroke = Stroke::new(1.0, NORD6);
    v.widgets.open.bg_fill = NORD2;
    v
}

/// Применяет выбранную тему к контексту egui.
pub(crate) fn apply_theme(ctx: &egui::Context, choice: ThemeChoice) {
    // Сбрасываем кастомные визуалы, чтобы Nord не «протекал» в другие темы.
    ctx.set_visuals_of(egui::Theme::Dark, egui::Visuals::dark());
    ctx.set_visuals_of(egui::Theme::Light, egui::Visuals::light());
    match choice {
        ThemeChoice::System => ctx.set_theme(egui::ThemePreference::System),
        ThemeChoice::Dark => ctx.set_theme(egui::ThemePreference::Dark),
        ThemeChoice::Light => ctx.set_theme(egui::ThemePreference::Light),
        ThemeChoice::Nord => {
            ctx.set_visuals_of(egui::Theme::Dark, nord_visuals());
            ctx.set_theme(egui::ThemePreference::Dark);
        }
    }
}
