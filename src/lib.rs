pub mod claude;
pub mod config;
pub mod pty;
pub mod tmux;
pub mod session;
pub mod tree;
pub mod keys;
pub mod ui;
pub mod app;
pub mod run;
pub mod viewer;
pub mod rows;

#[cfg(test)]
mod tests {
    #[test]
    fn crate_builds() {
        assert_eq!(2 + 2, 4);
    }
}
