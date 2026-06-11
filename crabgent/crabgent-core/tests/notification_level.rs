use crabgent_core::NotificationLevel;

const fn render_level(level: NotificationLevel) -> &'static str {
    match level {
        NotificationLevel::Info => "info",
        NotificationLevel::Warn => "warn",
        NotificationLevel::Error => "error",
        _ => "future",
    }
}

#[test]
fn external_match_uses_wildcard_for_future_levels() {
    assert_eq!(render_level(NotificationLevel::Info), "info");
    assert_eq!(render_level(NotificationLevel::Warn), "warn");
    assert_eq!(render_level(NotificationLevel::Error), "error");
}
