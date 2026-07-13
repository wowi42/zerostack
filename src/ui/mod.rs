mod event_handler;
pub(crate) mod events;
pub(crate) mod feed;
pub(crate) mod input;
pub(crate) mod markdown;
mod permission_handler;
pub(crate) mod pickers;
pub(crate) mod renderer;
pub(crate) mod slash;
pub(crate) mod statusline;
mod terminal;
pub(crate) mod utils;

pub mod app;

pub use app::App;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crossterm::event;
use crossterm::event::{KeyEventKind, MouseButton, MouseEventKind};
use crossterm::style::Color;
use tokio::sync::mpsc;

use crate::cli::Cli;
use crate::config::Config;
use crate::context::ContextFiles;
use crate::event::UserEvent;
#[cfg(feature = "mcp")]
use crate::extras::mcp::McpClientManager;
use crate::extras::status_signals::StatusSignals;
use crate::permission;
use crate::permission::ask::{AskReceiver, AskSender};
use crate::permission::checker::PermCheck;
use crate::provider::{AnyAgent, AnyClient};
use crate::sandbox::Sandbox;
use crate::session::{MessageRole, Session};
use crate::ui::pickers::rewind::RewindOutcome;
use crate::ui::renderer::copy_to_clipboard;

use self::utils::{parse_color, to_ansi_256};

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

pub(super) const C_AGENT: Color = Color::White;
pub(super) const C_ERROR: Color = Color::Red;
pub(super) const C_TOOL: Color = Color::Yellow;
pub(super) const C_PERM: Color = Color::Magenta;
pub(super) const C_BTW: Color = Color::Cyan;
#[cfg(feature = "advisor")]
pub(super) const C_HANDOFF: Color = Color::Green;

#[allow(clippy::too_many_arguments)]

pub(super) fn spawn_event_thread(
    user_tx: mpsc::Sender<UserEvent>,
    running: Arc<AtomicBool>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        while running.load(Ordering::Relaxed) {
            if let Ok(true) = event::poll(Duration::from_millis(50)) {
                match event::read() {
                    Ok(event::Event::Key(key)) => {
                        if key.kind == KeyEventKind::Press
                            && user_tx.blocking_send(UserEvent::Key(key)).is_err()
                        {
                            break;
                        }
                    }
                    Ok(event::Event::Mouse(m)) => match m.kind {
                        MouseEventKind::ScrollUp => {
                            if user_tx.blocking_send(UserEvent::ScrollUp).is_err() {
                                break;
                            }
                        }
                        MouseEventKind::ScrollDown => {
                            if user_tx.blocking_send(UserEvent::ScrollDown).is_err() {
                                break;
                            }
                        }
                        MouseEventKind::Down(MouseButton::Left) => {
                            let _ = user_tx.blocking_send(UserEvent::MouseDown {
                                row: m.row,
                                col: m.column,
                            });
                        }
                        MouseEventKind::Drag(MouseButton::Left) => {
                            let _ = user_tx.blocking_send(UserEvent::MouseDrag {
                                row: m.row,
                                col: m.column,
                            });
                        }
                        MouseEventKind::Up(MouseButton::Left) => {
                            let _ = user_tx.blocking_send(UserEvent::MouseUp {
                                row: m.row,
                                col: m.column,
                            });
                        }
                        _ => {}
                    },
                    Ok(event::Event::Resize(_cols, _rows)) => {
                        let _ = user_tx.blocking_send(UserEvent::Resize);
                    }
                    Ok(event::Event::Paste(data)) => {
                        let _ = user_tx.blocking_send(UserEvent::Paste(data));
                    }
                    Err(_) => break,
                    _ => {}
                }
            }
        }
    })
}

/// Lazily initialise the MCP client manager (connects only on first use).
#[cfg(feature = "mcp")]
pub(super) async fn ensure_mcp_manager<'a>(
    mcp: &'a mut Option<McpClientManager>,
    cfg: &'a Config,
) -> Option<&'a McpClientManager> {
    if mcp.is_none()
        && let Some(servers) = &cfg.mcp_servers
    {
        *mcp = Some(McpClientManager::connect_all(servers).await);
    }
    mcp.as_ref()
}

/// What to do with a submitted line, given whether a main run is already active.
/// Pure decision so it can be unit-tested without a TUI/agent.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SubmitAction {
    /// Idle: start a run now.
    Run,
    /// Running + plain text: queue and replay after the current run finishes.
    Queue,
    /// Running + a command (`/`, `.`, `!`): can't queue meaningfully — tell the
    /// user to wait or Ctrl-C.
    RejectWhileRunning,
    /// Empty submit: ignore.
    Ignore,
}

/// Commands that are safe to run *even while a main run is active* because they
/// don't spawn or mutate the main run — the single "bypass" whitelist. Add
/// future parallel-safe commands here. Currently: `/queue` (queue management)
/// and `/btw` (isolated, tool-less side question on its own event stream).
pub(crate) fn allowed_while_running(text: &str) -> bool {
    let t = text.trim_start();
    t == "/queue" || t.starts_with("/queue ") || t == "/btw" || t.starts_with("/btw ")
}

/// Build the rewind picker's list of `(message_index, preview)` for every user
/// turn in the conversation, oldest first. Only user turns are offered: a rewind
/// lands just before a message you sent, dropping everything after it.
pub(crate) fn rewind_targets(session: &Session) -> Vec<(usize, String)> {
    session
        .messages
        .iter()
        .enumerate()
        .filter(|(_, m)| m.role == MessageRole::User)
        .map(|(idx, m)| {
            let preview: String = m.content.chars().take(80).collect();
            (idx, preview.replace('\n', " "))
        })
        .collect()
}

pub(crate) fn classify_submission(is_running: bool, text: &str) -> SubmitAction {
    // Idle, or a whitelisted parallel-safe command → let it through to its
    // handler. Everything else, while running, is gated.
    if !is_running || allowed_while_running(text) {
        return SubmitAction::Run;
    }
    let t = text.trim_start();
    if t.is_empty() {
        SubmitAction::Ignore
    } else if t.starts_with('/') || t.starts_with('.') || t.starts_with('!') {
        SubmitAction::RejectWhileRunning
    } else {
        SubmitAction::Queue
    }
}

#[cfg(feature = "git-worktree")]
#[allow(clippy::too_many_arguments)]
/// Result of a background agent prebuild.
#[cfg(feature = "mcp")]
pub(super) type PrebuildPayload = (AnyAgent, Option<McpClientManager>);
#[cfg(not(feature = "mcp"))]
pub(super) type PrebuildPayload = AnyAgent;

/// If the background prebuild hasn't delivered yet, block until it does.
#[cfg(feature = "mcp")]
pub(super) async fn resolve_prebuild<'a>(
    agent: &'a mut Option<AnyAgent>,
    mcp_manager: &'a mut Option<McpClientManager>,
    prebuild_rx: &'a mut Option<mpsc::Receiver<PrebuildPayload>>,
) {
    if agent.is_some() {
        return;
    }
    if let Some(rx) = prebuild_rx.as_mut() {
        if let Some((a, mcp)) = rx.recv().await {
            *agent = Some(a);
            *mcp_manager = mcp;
        }
        *prebuild_rx = None;
    }
}

#[cfg(not(feature = "mcp"))]
pub(super) fn resolve_prebuild<'a>(
    agent: &'a mut Option<AnyAgent>,
    prebuild_rx: &'a mut Option<mpsc::Receiver<PrebuildPayload>>,
) -> impl std::future::Future<Output = ()> + 'a {
    async move {
        if agent.is_some() {
            return;
        }
        if let Some(rx) = prebuild_rx.as_mut() {
            if let Some(a) = rx.recv().await {
                *agent = Some(a);
            }
            *prebuild_rx = None;
        }
    }
}

/// Starts a single main agent run for `text` and records its abort handle.
/// The ONLY place that sets `agent_rx`/`is_running` for user-driven runs, so the
/// "at most one main run" invariant is enforced in one spot. Callers must ensure
/// no run is already active (otherwise the previous one would be orphaned).
#[allow(clippy::too_many_arguments)]

/// Continuation prompt injected after a mid-turn compaction. Hardcoded as a
/// `const` rather than a `prompts/*.md` file: every `.md` under `prompts/` is
/// loaded as a selectable mode, so a file here would pollute the prompt picker.
/// Acknowledging the compaction is deliberate — it frames the summary as "what
/// I already did," not as new user instructions. The narrow-tool-calls line is
/// always present because any mid-turn fire means the configured ceiling was
/// hit, so the urgency always applies.
pub(super) const MID_TURN_CONTINUE_PROMPT: &str = "[Context was compacted to save space; \
the full prior history is in the system summary above.]\n\nContinue with the \
user's original task. Do not redo work already completed per the summary; focus \
on what remains. Context was tight, so prefer narrower follow-up tool calls over \
wide ones until pressure subsides.";

/// Mid-turn auto-compaction (PR H). Invoked when real provider prompt pressure
/// (`CompletionCall` usage / context window) crosses
/// `mid_turn_compact_threshold`, and only when `compact_enabled` is true.
///
/// The in-flight run is aborted at the `CompletionCall` boundary — the model's
/// just-returned tool calls have not executed yet, so nothing is left half
/// applied. This turn's progress is recorded as a recap message (tool traffic
/// lives only in the now-aborted runner and never reaches the session, so
/// without this the agent would redo the turn), the session is compacted, and
/// the agent is respawned on the compacted history with a continuation prompt.
/// The dominant pressure relief is dropping the aborted run's in-flight tool
/// context, which the respawn achieves even when the session itself is under the
/// between-turn limit and `handle_compress` is a no-op.
#[allow(clippy::too_many_arguments)]

/// Hard stop for a turn whose context cannot be brought under the mid-turn
/// ceiling even after a compaction. What remains is the irreducible floor
/// (system prompt, tool schemas, kept-recent transcript, reserved response
/// space), so compacting again is futile. Aborts the run and shows the user the
/// full arithmetic — the model and context-window combination is simply too
/// small to run the agentic loop on this task.
#[allow(clippy::too_many_arguments)]

/// Builds the math-and-guidance body for a context-exhaustion stop. Pure (no
/// I/O) so the arithmetic can be unit-tested. `window` must be non-zero (the
/// caller only reaches here after gating on `context_window > 0`).
pub(crate) fn context_exhausted_report(
    prompt_tokens: u64,
    threshold: f64,
    window: u64,
    reserve: u64,
    keep_recent: u64,
) -> Vec<String> {
    let ceiling = (threshold * window as f64) as u64;
    let pressure_pct = prompt_tokens as f64 / window as f64 * 100.0;
    let overflow = prompt_tokens.saturating_sub(ceiling);
    vec![
        format!("  context window .............. {window} tokens"),
        format!(
            "  mid-turn ceiling ............ {ceiling} tokens  ({:.0}% of window)",
            threshold * 100.0
        ),
        format!(
            "  prompt after compaction ..... {prompt_tokens} tokens  ({pressure_pct:.0}% of window)"
        ),
        format!("  overflow above ceiling ...... {overflow} tokens"),
        format!("  reserved for response ....... {reserve} tokens"),
        format!("  kept-recent budget .......... {keep_recent} tokens"),
        String::new(),
        "This model and context-window combination is too small to run zerostack's \
         agentic loop on this task. To proceed you can:"
            .to_string(),
        "  - increase context_window (and the model server's real KV cache) so the \
         window clears the floor above;"
            .to_string(),
        format!(
            "  - raise mid_turn_compact_threshold above {pressure_pct:.0}% so this prompt \
             fits under the ceiling (trades safety for room: the real KV cache must still \
             hold {prompt_tokens}+ tokens);"
        ),
        "  - lower keep_recent_tokens or reserve_tokens to shrink the floor;".to_string(),
        "  - switch to a model/server with a larger context window, or split the task \
         into smaller pieces."
            .to_string(),
    ]
}

#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_arguments)]
pub async fn run_interactive(
    client: AnyClient,
    agent: Option<AnyAgent>,
    cli: &Cli,
    cfg: &Config,
    session: &mut Session,
    context: &mut ContextFiles,
    permission: Option<PermCheck>,
    ask_tx: Option<AskSender>,
    ask_rx: Option<AskReceiver>,
    sandbox: Sandbox,
    auto_trigger_msg: Option<String>,
    status_signals: Option<StatusSignals>,
    #[cfg(feature = "advisor")] handoff_rx: Option<crate::extras::advisor::HandoffReceiver>,
) -> anyhow::Result<()> {
    let mut app = App::new(
        client,
        agent,
        cli,
        cfg,
        session,
        context,
        permission,
        ask_tx,
        ask_rx,
        sandbox,
        status_signals,
        #[cfg(feature = "advisor")]
        handoff_rx,
    )
    .await?;
    app.run(auto_trigger_msg).await
}

#[cfg(feature = "advisor")]
pub(super) async fn handle_human_handoff(
    req: crate::extras::advisor::HandoffRequest,
    renderer: &mut Renderer,
    user_rx: &mut mpsc::Receiver<UserEvent>,
    agent_line_started: &mut bool,
    was_reasoning: &mut bool,
) -> anyhow::Result<()> {
    *was_reasoning = false;
    if *agent_line_started {
        renderer.write_line("", Color::White)?;
        *agent_line_started = false;
    }

    renderer.write_line("[handoff] Model requests your guidance:", C_HANDOFF)?;
    for line in req.question.lines() {
        renderer.write_line(&format!("  | {}", sanitize_output(line)), C_HANDOFF)?;
    }
    renderer.write_line("", C_HANDOFF)?;
    renderer.write_line(
        "  Type your response and press Enter (ESC to cancel):",
        C_HANDOFF,
    )?;

    let mut buffer = String::new();
    let response = loop {
        tokio::select! {
            Some(ev) = user_rx.recv() => {
                if let crate::event::UserEvent::Key(key) = ev {
                    match key.code {
                        crossterm::event::KeyCode::Enter => break buffer,
                        crossterm::event::KeyCode::Esc => break String::new(),
                        crossterm::event::KeyCode::Char(c) => {
                            buffer.push(c);
                            renderer.write_line(&format!("  > {}", buffer), C_HANDOFF)?;
                        }
                        crossterm::event::KeyCode::Backspace => {
                            buffer.pop();
                            renderer.write_line(&format!("  > {}", buffer), C_HANDOFF)?;
                        }
                        _ => {}
                    }
                }
            }
        }
    };

    if response.is_empty() {
        renderer.write_line("  [cancelled]", C_HANDOFF)?;
    } else {
        renderer.write_line(&format!("  [sent: {}]", response), C_HANDOFF)?;
    }
    renderer.write_line("", Color::White)?;

    let _ = req.reply.send(response);
    Ok(())
}
