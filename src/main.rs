pub mod acp;
mod app;
mod config;
mod git;
mod gui;
mod model;
mod os;
mod pager;

use std::path::PathBuf;

use clap::Parser;

/// The ASCII logo for lazygitrs
const LOGO: &str = include_str!("../logo.txt");

#[derive(Parser)]
#[command(name = "lazygitrs", version, about = "A fast and ergonomic terminal UI for git", before_help = LOGO)]
struct Cli {
    /// Path to the git repository
    #[arg(short, long)]
    path: Option<PathBuf>,

    /// Git work tree path
    #[arg(short = 'w', long = "work-tree")]
    work_tree: Option<PathBuf>,

    /// Git dir path
    #[arg(short = 'g', long = "git-dir")]
    git_dir: Option<PathBuf>,

    /// Enable debug logging
    #[arg(long)]
    debug: bool,

    /// Open directly in expanded diff view
    #[arg(short = 'd', long)]
    diff: bool,

    /// Specific file to focus in diff view (implies --diff)
    #[arg(short = 'f', long)]
    file: Option<String>,

    /// Print the default configuration YAML to stdout and exit
    #[arg(long)]
    print_default_config: bool,

    /// Configuration file or preset name to use (e.g. 'popup' or ~/.config/lazygitrs/config.yml)
    #[arg(short = 'c', long = "config")]
    config: Option<String>,

    /// Clear the active AI session for this repository and globally
    #[arg(long)]
    clear_session: bool,
}

/// Restore the terminal on panic so the user isn't left in raw mode + mouse
/// capture (which makes the shell unusable — every mouse move spews escape
/// sequences into the prompt).
fn install_panic_hook() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let mut stdout = std::io::stdout();
        let _ = crossterm::execute!(
            stdout,
            crossterm::event::DisableMouseCapture,
            crossterm::event::DisableFocusChange,
            crossterm::cursor::Show,
            crossterm::terminal::LeaveAlternateScreen,
        );
        let _ = crossterm::terminal::disable_raw_mode();
        prev(info);
    }));
}

fn main() {
    install_panic_hook();
    let cli = Cli::parse();

    if cli.print_default_config {
        let config = config::user_config::UserConfig::default();
        let yaml = serde_yaml::to_string(&config).unwrap();
        println!("{}", yaml);
        std::process::exit(0);
    }

    if cli.clear_session {
        // 1. Delete global fallback file
        if let Ok(home) = std::env::var("HOME") {
            let global_path = std::path::PathBuf::from(home).join(".lazygitrs_active_session.json");
            let _ = std::fs::remove_file(global_path);
        }

        // 2. Try to notify running instance for the current directory via API
        if let Ok(port_str) = std::fs::read_to_string(".lazygitrs.port") {
            if let Ok(port) = port_str.trim().parse::<u16>() {
                let url = format!("http://127.0.0.1:{}/session-api", port);
                let _ = std::process::Command::new("curl")
                    .args([
                        "-s",
                        "-X",
                        "POST",
                        &url,
                        "-H",
                        "content-type: application/json",
                        "--data",
                        r#"{"action":"unregister"}"#,
                    ])
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status();
            }
        }

        println!(
            "AI sessions cleared (global fallback removed, and local instance unregistered if running)."
        );
        std::process::exit(0);
    }

    // Set up logging if debug mode
    if cli.debug {
        tracing_subscriber::fmt()
            .with_env_filter("lazygitrs=debug")
            .with_writer(std::io::stderr)
            .init();
    }

    let repo_path = cli
        .path
        .or(cli.work_tree)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    let start_in_diff = cli.diff || cli.file.is_some();

    match app::App::new(repo_path, cli.debug, start_in_diff, cli.file, cli.config) {
        Ok(app) => {
            if let Err(e) = app.run() {
                eprintln!("Error: {:#}", e);
                std::process::exit(1);
            }
        }
        Err(e) => {
            eprintln!("Error: {:#}", e);
            std::process::exit(1);
        }
    }
}
