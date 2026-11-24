//! Плавающее окно графа связей.

use std::path::PathBuf;

use eframe::egui;

use crate::state::VaultState;

/// Рисует окно графа; возвращает заметку, выбранную кликом по узлу.
pub(crate) fn show(ui: &mut egui::Ui, vs: &mut VaultState) -> Option<PathBuf> {
    let mut open = true;
    let mut clicked: Option<PathBuf> = None;
    egui::Window::new("Граф связей")
        .open(&mut open)
        .default_size([520.0, 420.0])
        .resizable(true)
        .show(ui.ctx(), |ui| {
            // Граф пересобирается только пока окно открыто (C3, `refresh_graph`).
            vs.refresh_graph();
            // Раскладываем, пока симуляция не успокоится; после этого кадры
            // не нужны и процессор свободен.
            if vs.graph.animate(3) {
                ui.ctx().request_repaint();
            }
            let (response, painter) =
                ui.allocate_painter(ui.available_size(), egui::Sense::click());
            let rect = response.rect;
            if rect.width() < 2.0 || rect.height() < 2.0 {
                return;
            }
            painter.rect_filled(rect, 0.0, ui.visuals().extreme_bg_color);

            let n = vs.graph.node_count();
            if n == 0 {
                painter.text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "Пока нет заметок",
                    egui::FontId::proportional(14.0),
                    ui.visuals().weak_text_color(),
                );
                return;
            }

            // Границы позиций и масштаб.
            let mut min = egui::Vec2::splat(f32::INFINITY);
            let mut max = egui::Vec2::splat(f32::NEG_INFINITY);
            for i in 0..n {
                if let Some((_, _, p)) = vs.graph.node(i) {
                    let p = p.to_vec2();
                    min = min.min(p);
                    max = max.max(p);
                }
            }
            let size = (max - min).max(egui::Vec2::splat(1.0));
            let scale = ((rect.width() - 60.0) / size.x)
                .min((rect.height() - 60.0) / size.y)
                .min(4.0);
            let offset = rect.center() - (min + max) * 0.5 * scale;
            let map =
                |p: egui::Pos2| egui::Pos2::new(offset.x + p.x * scale, offset.y + p.y * scale);

            // Рёбра.
            let stroke =
                egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color);
            for (a, b) in vs.graph.edge_pairs() {
                let (Some((_, _, pa)), Some((_, _, pb))) = (vs.graph.node(a), vs.graph.node(b))
                else {
                    continue;
                };
                painter.line_segment([map(pa), map(pb)], stroke);
            }

            // Узлы. При большой сети подписи у всех узлов превращаются в кашу,
            // поэтому их видно только у открытой заметки и у узла под курсором.
            let many_nodes = n > 300;
            let hover_pos = response.hover_pos();
            let click_pos = response
                .clicked()
                .then(|| response.interact_pointer_pos())
                .flatten()
                .unwrap_or(rect.center());
            for i in 0..n {
                let Some((title, path, p)) = vs.graph.node(i) else {
                    continue;
                };
                let center = map(p);
                let is_open = vs.open.as_deref() == Some(path);
                let color = if is_open {
                    ui.visuals().selection.bg_fill
                } else {
                    ui.visuals().widgets.inactive.fg_stroke.color
                };
                painter.circle_filled(center, if is_open { 7.0 } else { 5.0 }, color);
                let near_cursor = hover_pos.is_some_and(|h| (h - center).length() < 14.0);
                if !many_nodes || is_open || near_cursor {
                    painter.text(
                        center + egui::vec2(0.0, 10.0),
                        egui::Align2::CENTER_TOP,
                        title,
                        egui::FontId::proportional(11.0),
                        ui.visuals().text_color(),
                    );
                }
                if response.clicked() && (click_pos - center).length() < 14.0 {
                    clicked = Some(path.to_path_buf());
                }
            }
        });

    vs.show_graph = open;
    clicked
}
