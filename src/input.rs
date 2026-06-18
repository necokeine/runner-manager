use crossterm::event::{KeyCode, KeyEvent};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Quit,
    Up,
    Down,
    Activate,
    OpenSession,
    Kill,
    Noop,
}

pub fn map_key(key: KeyEvent) -> Action {
    match key.code {
        KeyCode::Char('q') => Action::Quit,
        KeyCode::Char('j') | KeyCode::Down => Action::Down,
        KeyCode::Char('k') | KeyCode::Up => Action::Up,
        KeyCode::Enter => Action::Activate,
        KeyCode::Char('a') => Action::OpenSession,
        KeyCode::Char('x') => Action::Kill,
        _ => Action::Noop,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn maps_navigation_and_commands() {
        assert_eq!(map_key(key(KeyCode::Char('q'))), Action::Quit);
        assert_eq!(map_key(key(KeyCode::Char('j'))), Action::Down);
        assert_eq!(map_key(key(KeyCode::Down)), Action::Down);
        assert_eq!(map_key(key(KeyCode::Char('k'))), Action::Up);
        assert_eq!(map_key(key(KeyCode::Up)), Action::Up);
        assert_eq!(map_key(key(KeyCode::Enter)), Action::Activate);
        assert_eq!(map_key(key(KeyCode::Char('a'))), Action::OpenSession);
        assert_eq!(map_key(key(KeyCode::Char('x'))), Action::Kill);
        assert_eq!(map_key(key(KeyCode::Char('z'))), Action::Noop);
    }
}
