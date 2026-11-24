//! Модалка быстрого перехода (Ctrl+O).

use eframe::egui;

use crate::state::VaultState;

/// Нажатия клавиш, прочитанные в начале кадра до того, как их поглотят виджеты.
#[derive(Clone, Copy, Default)]
pub(crate) struct Keys {
    pub(crate) esc: bool,
    pub(crate) enter: bool,
    pub(crate) up: bool,
    pub(crate) down: bool,
}

/// Рисует модалку, если она открыта; обрабатывает выбор и закрытие.
pub(crate) fn show(ui: &mut egui::Ui, vs: &mut VaultState, keys: Keys) {
    if vs.quick_open.is_none() {
        return;
    }
    let modal = egui::Modal::new(egui::Id::unique("quick_open"))
        .area(
            egui::Area::new(egui::Id::unique("quick_open_area"))
                .kind(egui::UiKind::Modal)
                .anchor(egui::Align2::CENTER_TOP, egui::Vec2::ZERO)
                .order(egui::Order::Foreground)
                .interactable(true)
                .sense(egui::Sense::hover()),
        )
        .show(ui.ctx(), |ui| {
            let qo = vs.quick_open.as_mut().unwrap();
            ui.add_space(60.0);
            ui.set_min_width(420.0);
            ui.strong("Быстрый переход");
            let text_out = ui.add(
                egui::TextEdit::singleline(&mut qo.query)
                    .hint_text("Название заметки…")
                    .desired_width(f32::INFINITY)
                    .id_salt("quick_input"),
            );
            text_out.request_focus();
            if text_out.changed() {
                qo.selected = 0;
            }

            let entries: Vec<(std::path::PathBuf, String)> = {
                let q = qo.query.trim().to_lowercase();
                vs.index
                    .switcher_entries()
                    .into_iter()
                    .filter(|(p, t)| {
                        q.is_empty()
                            || t.to_lowercase().contains(&q)
                            || p.to_string_lossy().to_lowercase().contains(&q)
                    })
                    .take(20)
                    .collect()
            };

            let mut chosen: Option<std::path::PathBuf> = None;
            let mut close = false;
            if keys.down {
                qo.selected = (qo.selected + 1).min(entries.len().saturating_sub(1));
            }
            if keys.up {
                qo.selected = qo.selected.saturating_sub(1);
            }
            if keys.enter {
                if let Some((p, _)) = entries.get(qo.selected) {
                    chosen = Some(p.clone());
                    close = true;
                }
            }
            if keys.esc {
                close = true;
            }

            ui.separator();
            if entries.is_empty() {
                ui.weak("Ничего не найдено");
            }
            egui::ScrollArea::vertical()
                .id_salt("quick_list")
                .max_height(320.0)
                .show(ui, |ui| {
                    for (i, (p, t)) in entries.iter().enumerate() {
                        let label = format!("{t}  ·  {}", p.display());
                        if ui.selectable_label(i == qo.selected, label).clicked() {
                            chosen = Some(p.clone());
                            close = true;
                        }
                    }
                });
            ui.separator();
            ui.weak("Enter — открыть · Esc — закрыть");

            (chosen, close)
        });

    if let Some(p) = modal.inner.0 {
        vs.load(p);
    }
    let closed = modal.inner.1 || modal.backdrop_response.clicked();
    if closed {
        vs.quick_open = None;
    }
}
