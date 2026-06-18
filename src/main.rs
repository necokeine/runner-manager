use std::env;

use runner_manager::bootstrap;
use runner_manager::cli::{parse_mode, Mode};
use runner_manager::run;

fn main() -> std::io::Result<()> {
    let args: Vec<String> = env::args().skip(1).collect();
    match parse_mode(&args) {
        Mode::Tui => {
            let root = env::current_dir()?;
            let editor = env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());
            run::run(root, "runner".to_string(), editor)
        }
        Mode::Bootstrap => bootstrap::run_bootstrap("runner", "runner-manager"),
    }
}
