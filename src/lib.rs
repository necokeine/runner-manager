pub mod tmux;
pub mod session;
pub mod tree;
pub mod input;
pub mod ui;
pub mod app;
pub mod run;
pub mod bootstrap;
pub mod cli;

#[cfg(test)]
mod tests {
    #[test]
    fn crate_builds() {
        assert_eq!(2 + 2, 4);
    }
}
