use crossterm::style::Color;

use crate::context::ContextFiles;
use crate::permission;
use crate::permission::checker::PermCheck;
use crate::session::Session;

use super::C_AGENT;

/// Apply the mode directive embedded in `current_prompt` (if any) to the permission checker.
pub(crate) fn apply_current_prompt_mode(
    context: &mut ContextFiles,
    permission: &Option<PermCheck>,
) {
    let Some(content) = &context.current_prompt.clone() else {
        return;
    };
    let (mode_directive, clean_content) = permission::parse_prompt_mode(content);
    if mode_directive.is_some() {
        context.current_prompt = Some(clean_content.to_string());
    }
    let Some(mode_str) = mode_directive else {
        return;
    };
    let Some(perm) = permission else { return };
    let mut guard = perm.lock().unwrap_or_else(|e| e.into_inner());
    if mode_str == "last_user_mode" {
        guard.restore_user_mode();
    } else if let Some(mode) = permission::SecurityMode::from_str(mode_str) {
        guard.set_prompt_mode(mode);
    }
}

/// Switch the active prompt to `prompt_name`, applying any embedded mode directive.
/// Returns `true` if the prompt was found and applied.
pub(crate) fn switch_prompt(
    context: &mut ContextFiles,
    permission: &Option<PermCheck>,
    prompt_name: &str,
) -> bool {
    let content = match context.prompts.get(prompt_name).cloned() {
        Some(c) => c,
        None => return false,
    };
    let (mode_directive_str, clean_content) = permission::parse_prompt_mode(&content);
    let mode_directive = mode_directive_str.map(|s| s.to_string());
    context.current_prompt = Some(if mode_directive.is_some() {
        clean_content.to_string()
    } else {
        content
    });
    context.current_prompt_name = Some(prompt_name.to_string());
    if let Some(ref mode_str) = mode_directive
        && let Some(perm) = permission {
            let mut guard = perm.lock().unwrap_or_else(|e| e.into_inner());
            if mode_str == "last_user_mode" {
                guard.restore_user_mode();
            } else if let Some(mode) = permission::SecurityMode::from_str(mode_str) {
                guard.set_prompt_mode(mode);
            }
        }
    true
}

/// Write a "switched to prompt '...'" message to the renderer and save the session.
pub(crate) fn report_prompt_switch(
    renderer: &mut crate::ui::renderer::Renderer,
    session: &Session,
    prompt_name: &str,
    cli: &crate::cli::Cli,
) {
    let _ = renderer.write_line(&format!("switched to prompt '{}'", prompt_name), C_AGENT);
    if !cli.no_session
        && let Err(e) = crate::session::storage::save_session(session)
    {
        let _ = renderer.write_line(
            &format!("warning: failed to save session: {}", e),
            Color::Red,
        );
    }
}
