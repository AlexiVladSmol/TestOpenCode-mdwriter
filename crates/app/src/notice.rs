//! Уведомления пользователю: ошибки и информационные сообщения.
//!
//! В release-сборке под Windows консоли нет (`windows_subsystem = "windows"`),
//! поэтому всё, что пользователь должен узнать (ошибка сохранения, чтения,
//! экспорта…), показывается в окне. `Notices` — дешёвый в клонировании
//! хэндл на общую очередь: им владеет `App`, а клон получает `VaultState`.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use eframe::egui;

/// Сколько секунд висит информационное уведомление.
const INFO_TTL: f64 = 5.0;
/// Сколько секунд висит ошибка, если пользователь её не закрыл.
/// Дольше, чем Info: ошибку надо успеть прочитать, но и висеть вечно она не
/// должна — иначе старая ошибка «догоняет» пользователя через несколько
/// успешных действий. Ошибки с ключом (`error_keyed`) — это состояние
/// (например, «не сохраняется заметка»), их снимает только `clear_key`.
const ERROR_TTL: f64 = 20.0;
/// Максимум одновременно показанных уведомлений.
const MAX_ITEMS: usize = 5;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NoticeLevel {
    Info,
    Error,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Notice {
    pub level: NoticeLevel,
    pub text: String,
    /// Сколько раз подряд пришло это же сообщение.
    pub repeats: u32,
    /// Время первого показа (`egui` `input.time`); `None` — ещё не показано.
    shown_at: Option<f64>,
    /// Ключ «темы» уведомления (например, `save:<путь>`): такие уведомления
    /// обновляются на месте и снимаются через [`Notices::clear_key`].
    key: Option<String>,
}

/// Общая очередь уведомлений.
#[derive(Clone, Default)]
pub struct Notices(Rc<RefCell<Vec<Notice>>>);

impl Notices {
    pub fn info(&self, text: impl Into<String>) {
        self.push(NoticeLevel::Info, text.into());
    }

    pub fn error(&self, text: impl Into<String>) {
        self.push(NoticeLevel::Error, text.into());
    }

    /// Ошибка с ключом: повтор с тем же ключом (где бы он ни был в очереди)
    /// обновляет текст существующего уведомления и увеличивает счётчик.
    pub fn error_keyed(&self, key: &str, text: impl Into<String>) {
        let text = text.into();
        eprintln!("[Error] {text}");
        let mut items = self.0.borrow_mut();
        if let Some(n) = items.iter_mut().find(|n| n.key.as_deref() == Some(key)) {
            n.level = NoticeLevel::Error;
            n.text = text;
            n.repeats += 1;
            n.shown_at = None;
            return;
        }
        items.push(Notice {
            level: NoticeLevel::Error,
            text,
            repeats: 1,
            shown_at: None,
            key: Some(key.to_string()),
        });
        Self::evict(&mut items);
    }

    /// Убирает уведомления с ключом `key` (проблема разрешилась).
    /// Возвращает, было ли что убирать.
    pub fn clear_key(&self, key: &str) -> bool {
        let mut items = self.0.borrow_mut();
        let before = items.len();
        items.retain(|n| n.key.as_deref() != Some(key));
        items.len() != before
    }

    /// Добавляет уведомление. Повтор последнего сообщения не создаёт новое,
    /// а увеличивает счётчик и перезапускает таймер скрытия.
    pub fn push(&self, level: NoticeLevel, text: String) {
        eprintln!("[{level:?}] {text}");
        let mut items = self.0.borrow_mut();
        if let Some(last) = items.last_mut() {
            if last.level == level && last.text == text && last.key.is_none() {
                last.repeats += 1;
                last.shown_at = None;
                return;
            }
        }
        items.push(Notice {
            level,
            text,
            repeats: 1,
            shown_at: None,
            key: None,
        });
        Self::evict(&mut items);
    }

    fn evict(items: &mut Vec<Notice>) {
        // Переполнение: сначала вытесняем самые старые Info, потом что угодно.
        while items.len() > MAX_ITEMS {
            let idx = items
                .iter()
                .position(|n| n.level == NoticeLevel::Info)
                .unwrap_or(0);
            items.remove(idx);
        }
    }

    /// Копия текущих уведомлений (для отрисовки и тестов).
    pub fn items(&self) -> Vec<Notice> {
        self.0.borrow().clone()
    }

    pub fn dismiss(&self, idx: usize) {
        let mut items = self.0.borrow_mut();
        if idx < items.len() {
            items.remove(idx);
        }
    }

    /// Отмечает время первого показа и убирает истёкшие Info.
    /// Возвращает, через сколько секунд истечёт ближайшее Info (для перерисовки).
    pub fn tick(&self, now: f64) -> Option<f64> {
        let mut items = self.0.borrow_mut();
        for n in items.iter_mut() {
            n.shown_at.get_or_insert(now);
        }
        items.retain(|n| match expiry(n) {
            // Ключевые ошибки живут, пока проблема не разрешена.
            None => true,
            Some(ttl) => now - n.shown_at.unwrap_or(now) < ttl,
        });
        items
            .iter()
            .filter_map(|n| expiry(n).map(|ttl| ttl - (now - n.shown_at.unwrap_or(now))))
            .reduce(f64::min)
    }

    /// Рисует уведомления в правом нижнем углу окна.
    pub fn show(&self, ctx: &egui::Context, now: f64) {
        if let Some(remaining) = self.tick(now) {
            ctx.request_repaint_after(Duration::from_secs_f64(remaining.max(0.05)));
        }
        let items = self.items();
        if items.is_empty() {
            return;
        }

        let mut dismissed: Option<usize> = None;
        egui::Area::new(egui::Id::unique("mdwriter_notices"))
            .anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-12.0, -12.0))
            .order(egui::Order::Foreground)
            .interactable(true)
            .show(ctx, |ui| {
                ui.set_max_width(380.0);
                // Только символы, которые есть во встроенных шрифтах egui
                // (значки вроде ⚠/✕ рисуются «квадратиками»).
                for (i, n) in items.iter().enumerate() {
                    let (prefix, color) = match n.level {
                        NoticeLevel::Info => ("", ui.visuals().text_color()),
                        NoticeLevel::Error => ("Ошибка: ", ui.visuals().error_fg_color),
                    };
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.horizontal_top(|ui| {
                            let mut text = format!("{prefix}{}", n.text);
                            if n.repeats > 1 {
                                text.push_str(&format!(" (×{})", n.repeats));
                            }
                            ui.add(egui::Label::new(egui::RichText::new(text).color(color)).wrap());
                            if ui.small_button("x").on_hover_text("Скрыть").clicked() {
                                dismissed = Some(i);
                            }
                        });
                    });
                    ui.add_space(4.0);
                }
            });
        if let Some(i) = dismissed {
            self.dismiss(i);
        }
    }
}

/// Через сколько секунд уведомление скрывается; `None` — само не исчезнет
/// (ключевые ошибки снимаются `clear_key`, когда проблема разрешилась).
fn expiry(n: &Notice) -> Option<f64> {
    match n.level {
        NoticeLevel::Info => Some(INFO_TTL),
        NoticeLevel::Error if n.key.is_some() => None,
        NoticeLevel::Error => Some(ERROR_TTL),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_message_is_collapsed() {
        let n = Notices::default();
        n.error("не удалось сохранить");
        n.error("не удалось сохранить");
        n.info("другое");
        n.error("не удалось сохранить");
        let items = n.items();
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].repeats, 2);
        assert_eq!(items[2].repeats, 1);
    }

    #[test]
    fn info_expires_first_then_plain_error() {
        let n = Notices::default();
        n.info("инфо");
        n.error("ошибка");
        // Ближайший срок — у Info.
        assert_eq!(n.tick(10.0), Some(INFO_TTL));
        assert_eq!(n.items().len(), 2);
        // Info исчезла, ошибка ещё видна и теперь её срок — ближайший.
        let left = n.tick(10.0 + INFO_TTL + 0.1);
        assert!(left.is_some_and(|s| (s - (ERROR_TTL - INFO_TTL - 0.1)).abs() < 1e-6));
        assert_eq!(n.items().len(), 1);
        assert_eq!(n.items()[0].level, NoticeLevel::Error);
        // Обычная ошибка тоже уходит сама, а не висит до конца сеанса.
        assert_eq!(n.tick(10.0 + ERROR_TTL + 0.1), None);
        assert!(n.items().is_empty());
    }

    #[test]
    fn keyed_error_stays_until_cleared() {
        let n = Notices::default();
        n.error_keyed("save:a.md", "Не удалось сохранить a.md");
        assert_eq!(n.tick(0.0), None, "у ключевой ошибки нет таймера");
        assert_eq!(n.tick(ERROR_TTL * 10.0), None);
        assert_eq!(n.items().len(), 1);
        assert!(n.clear_key("save:a.md"));
        assert!(n.items().is_empty());
    }

    #[test]
    fn repeat_restarts_info_timer() {
        let n = Notices::default();
        n.info("инфо");
        n.tick(0.0);
        n.info("инфо");
        // Повтор пришёл позже — отсчёт начинается с момента нового показа.
        n.tick(4.0);
        n.tick(8.0);
        assert_eq!(n.items().len(), 1);
        n.tick(9.5);
        assert!(n.items().is_empty());
    }

    #[test]
    fn overflow_evicts_info_first() {
        let n = Notices::default();
        n.error("e0");
        n.info("i0");
        for i in 1..MAX_ITEMS {
            n.error(format!("e{i}"));
        }
        let items = n.items();
        assert_eq!(items.len(), MAX_ITEMS);
        assert!(items.iter().all(|x| x.level == NoticeLevel::Error));
        assert_eq!(items[0].text, "e0");
    }

    #[test]
    fn keyed_error_updates_in_place_and_clears() {
        let n = Notices::default();
        n.error_keyed("save:a.md", "Не удалось сохранить a.md: доступ");
        n.info("другое");
        n.error_keyed("save:a.md", "Не удалось сохранить a.md: диск полон");
        let items = n.items();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].repeats, 2);
        assert!(items[0].text.ends_with("диск полон"));

        assert!(n.clear_key("save:a.md"));
        assert!(!n.clear_key("save:a.md"));
        let items = n.items();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].text, "другое");
    }

    #[test]
    fn plain_error_does_not_merge_into_keyed() {
        let n = Notices::default();
        n.error_keyed("save:a.md", "текст");
        n.error("текст");
        assert_eq!(n.items().len(), 2);
    }

    #[test]
    fn dismiss_removes_item() {
        let n = Notices::default();
        n.error("a");
        n.error("b");
        n.dismiss(0);
        n.dismiss(5); // вне диапазона — без паники
        assert_eq!(n.items().len(), 1);
        assert_eq!(n.items()[0].text, "b");
    }
}
