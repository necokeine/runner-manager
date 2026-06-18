#[derive(Debug, PartialEq, Eq)]
pub enum Mode {
    Bootstrap,
    Tui,
}

pub fn parse_mode(args: &[String]) -> Mode {
    match args.first().map(|s| s.as_str()) {
        Some("tui") => Mode::Tui,
        _ => Mode::Bootstrap,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_args_is_bootstrap() {
        assert_eq!(parse_mode(&[]), Mode::Bootstrap);
    }

    #[test]
    fn tui_arg_selects_tui() {
        assert_eq!(parse_mode(&["tui".to_string()]), Mode::Tui);
    }

    #[test]
    fn unknown_arg_is_bootstrap() {
        assert_eq!(parse_mode(&["wat".to_string()]), Mode::Bootstrap);
    }
}
