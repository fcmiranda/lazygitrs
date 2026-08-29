use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent};

use crate::config::KeybindingConfig;
use crate::gui::Gui;
use crate::gui::popup::{PopupState, make_textarea};

pub fn handle_key(gui: &mut Gui, key: KeyEvent, _keybindings: &KeybindingConfig) -> Result<()> {
    // Switch to worktree
    if key.code == KeyCode::Char(' ') {
        return switch_worktree(gui);
    }

    // Create new worktree
    if key.code == KeyCode::Char('n') {
        return create_worktree(gui);
    }

    // Remove worktree
    if key.code == KeyCode::Char('d') {
        return remove_worktree(gui);
    }

    Ok(())
}

fn switch_worktree(gui: &mut Gui) -> Result<()> {
    let selected = gui.context_mgr.selected_active();
    let model = gui.model.lock().unwrap();
    if let Some(wt) = model.worktrees.get(selected) {
        if wt.is_current {
            return Ok(()); // Already in this worktree
        }
        let path = wt.path.clone();
        let branch = wt.branch.clone();
        drop(model);

        gui.popup = PopupState::Confirm {
            title: "Switch worktree".to_string(),
            message: format!(
                "Open lazygitrs in worktree '{}' ({})?\nThis will launch a new instance.",
                branch, path
            ),
            on_confirm: Box::new(move |gui| {
                // Spawn a new lazygitrs instance in the worktree directory
                let exe = std::env::current_exe().unwrap_or_else(|_| "lazygitrs".into());
                std::process::Command::new(exe)
                    .arg("--path")
                    .arg(&path)
                    .spawn()?;
                gui.should_quit = true;
                Ok(())
            }),
        };
    }
    Ok(())
}

fn create_worktree(gui: &mut Gui) -> Result<()> {
    gui.popup = PopupState::Input {
        title: "New worktree branch name".to_string(),
        textarea: make_textarea(""),
        on_confirm: Box::new(|gui, input| {
            let parts: Vec<&str> = input.split_whitespace().collect();
            if parts.is_empty() {
                return Ok(());
            }

            let branch = parts[0];
            let base = if parts.len() >= 2 {
                Some(parts[1])
            } else {
                None
            };

            match gui.git.add_worktree(branch, base, None) {
                Ok(target_path) => {
                    gui.needs_refresh = true;
                    let path_display = target_path.display().to_string();
                    let branch_name = branch.to_string();

                    // Prompt to switch to newly created worktree
                    gui.popup = PopupState::Confirm {
                        title: "Switch to worktree".to_string(),
                        message: format!(
                            "Worktree created for '{}' at:\n{}\n\nOpen lazygitrs in new worktree?",
                            branch_name, path_display
                        ),
                        on_confirm: Box::new(move |gui| {
                            let exe =
                                std::env::current_exe().unwrap_or_else(|_| "lazygitrs".into());
                            std::process::Command::new(exe)
                                .arg("--path")
                                .arg(&path_display)
                                .spawn()?;
                            gui.should_quit = true;
                            Ok(())
                        }),
                    };
                }
                Err(e) => {
                    gui.popup = PopupState::Message {
                        title: "Create worktree error".to_string(),
                        message: format!("{}", e),
                        kind: crate::gui::popup::MessageKind::Error,
                    };
                }
            }
            Ok(())
        }),
        is_commit: false,
        confirm_focused: false,
    };
    Ok(())
}

fn remove_worktree(gui: &mut Gui) -> Result<()> {
    let selected = gui.context_mgr.selected_active();
    let model = gui.model.lock().unwrap();
    if let Some(wt) = model.worktrees.get(selected) {
        if wt.is_current || wt.is_main {
            return Ok(()); // Can't remove current or main worktree
        }
        let path = wt.path.clone();
        let branch = wt.branch.clone();
        drop(model);

        gui.popup = PopupState::Confirm {
            title: "Remove worktree".to_string(),
            message: format!(
                "Remove worktree '{}' ({})?\nThis won't delete the branch.",
                branch, path
            ),
            on_confirm: Box::new(move |gui| {
                gui.git.remove_worktree(&path, false)?;
                gui.needs_refresh = true;
                Ok(())
            }),
        };
    }
    Ok(())
}
