//! Дата для ежедневной заметки.

use chrono::{Local, NaiveDate};

/// Текущая дата по **локальному** времени в формате `ГГГГ-ММ-ДД`.
/// UTC здесь не годится: в Москве с 00:00 до 03:00 «Today» открывал бы
/// вчерашнюю заметку.
pub(crate) fn today_str() -> String {
    date_str(Local::now().date_naive())
}

/// Форматирует дату как `ГГГГ-ММ-ДД`.
pub(crate) fn date_str(d: NaiveDate) -> String {
    d.format("%Y-%m-%d").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn date_str_formats_iso() {
        let d = date_str(NaiveDate::from_ymd_opt(2026, 1, 5).unwrap());
        assert_eq!(d, "2026-01-05");
        let d = date_str(NaiveDate::from_ymd_opt(2026, 12, 31).unwrap());
        assert_eq!(d, "2026-12-31");
    }
}
