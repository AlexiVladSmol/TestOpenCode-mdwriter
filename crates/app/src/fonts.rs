//! Жирный системный шрифт для заголовков предпросмотра.

use eframe::egui;

/// Пытается найти жирный системный шрифт (Windows/Linux).
pub(crate) fn load_bold_font() -> Option<egui::FontData> {
    const CANDIDATES: &[&str] = &[
        // Windows
        "C:\\Windows\\Fonts\\segoeuib.ttf",
        "C:\\Windows\\Fonts\\arialbd.ttf",
        "C:\\Windows\\Fonts\\verdanab.ttf",
        "C:\\Windows\\Fonts\\tahomabd.ttf",
        // Linux
        "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
        "/usr/share/fonts/truetype/liberation/LiberationSans-Bold.ttf",
        "/usr/share/fonts/truetype/noto/NotoSans-Bold.ttf",
        "/usr/share/fonts/noto/NotoSans-Bold.ttf",
        "/usr/share/fonts/TTF/DejaVuSans-Bold.ttf",
    ];
    for p in CANDIDATES {
        if let Ok(bytes) = std::fs::read(p) {
            return Some(egui::FontData::from_owned(bytes));
        }
    }
    None
}
