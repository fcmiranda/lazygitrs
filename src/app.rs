use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::config::AppConfig;
use crate::git::GitCommands;
use crate::gui::Gui;

pub struct App {
    pub config: AppConfig,
    pub repo_path: PathBuf,
    pub start_in_diff: bool,
    pub filter_file: Option<String>,
    pub is_popup: bool,
}

impl App {
    pub fn new(
        repo_path: PathBuf,
        debug: bool,
        start_in_diff: bool,
        filter_file: Option<String>,
        config_override: Option<String>,
    ) -> Result<Self> {
        let is_popup = config_override.as_deref() == Some("popup");
        let config = AppConfig::load(debug, config_override)?;

        // Validate git repo
        if !GitCommands::is_valid_repo(&repo_path) {
            anyhow::bail!("'{}' is not a git repository", repo_path.display());
        }

        Ok(Self {
            config,
            repo_path,
            start_in_diff,
            filter_file,
            is_popup,
        })
    }

    pub fn run(mut self) -> Result<()> {
        let git = GitCommands::new(&self.repo_path).context("Failed to initialize git commands")?;

        // Update recent repos
        let repo_str = git.repo_path().to_string_lossy().to_string();
        self.config.app_state.add_recent_repo(&repo_str);
        let _ = self.config.save_state();

        let mut gui = Gui::new(
            self.config,
            git,
            self.start_in_diff,
            self.filter_file,
            self.is_popup,
        )?;
        gui.run()?;

        Ok(())
    }
}
