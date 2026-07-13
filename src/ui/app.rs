use std::collections::VecDeque;
use std::io;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use compact_str::CompactString;
use tokio::sync::mpsc;

use crate::cli::Cli;
use crate::config::Config;
use crate::context::ContextFiles;
use crate::event::{AgentEvent, UserEvent};
use crate::provider::{AnyAgent, AnyClient};
use crate::sandbox::Sandbox;
use crate::session::Session;

use super::input::InputEditor;
use super::renderer::Renderer;
use super::statusline;
use super::terminal::TerminalGuard;

#[cfg(feature = "advisor")]
use crate::extras::advisor;
#[cfg(feature = "loop")]
use crate::extras::r#loop::LoopState;
#[cfg(feature = "mcp")]
use crate::extras::mcp::McpClientManager;

use super::{PrebuildPayload, parse_color, refresh_display, spawn_event_thread, to_ansi_256};

/// State related to an active agent run.
#[derive(Default)]
pub struct AgentRunState {
    pub is_running: bool,
    pub agent_rx: Option<mpsc::Receiver<AgentEvent>>,
    pub main_abort: Option<tokio::task::AbortHandle>,
    pub pending_inputs: VecDeque<String>,
    pub agent_line_started: bool,
    pub response_buf: String,
    pub response_start_line: Option<usize>,
    pub was_reasoning: bool,
    pub pending_send: Option<String>,
    pub turn_trace: Vec<CompactString>,
    pub awaiting_compaction_relief: bool,
}

/// State for chain-of-prompts transitions.
#[derive(Default)]
pub struct ChainState {
    pub pending: Option<crate::extras::chain::ChainPhase>,
    pub label_msg: Option<String>,
    pub dot_prompt_restore: Option<String>,
}

/// State for `/btw` side questions.
pub struct BtwState {
    pub tx: mpsc::Sender<crate::event::BtwEvent>,
    pub abort: Vec<(u32, tokio::task::AbortHandle)>,
    pub inflight: usize,
    pub next_id: u32,
    pub total_cost: f64,
    pub total_in: u64,
    pub total_out: u64,
}

impl BtwState {
    pub fn new() -> (Self, mpsc::Receiver<crate::event::BtwEvent>) {
        let (tx, rx) = mpsc::channel(32);
        (
            Self {
                tx,
                abort: Vec::new(),
                inflight: 0,
                next_id: 0,
                total_cost: 0.0,
                total_in: 0,
                total_out: 0,
            },
            rx,
        )
    }
}

/// UI-related context: renderer, input, and display settings.
pub struct UiContext {
    pub renderer: Renderer,
    pub input: InputEditor,
    pub show_reasoning: bool,
    pub reasoning_enabled: bool,
    pub todo_tools_enabled: bool,
    pub last_branch_check: std::time::Instant,
}

/// The main application struct for the interactive TUI.
/// Holds all state, channels, and provides methods for event dispatch,
/// agent lifecycle, and slash command handling.
pub struct App<'a> {
    pub client: AnyClient,
    pub agent: Option<AnyAgent>,
    pub cli: &'a Cli,
    pub cfg: &'a Config,
    pub session: &'a mut Session,
    pub context: &'a mut ContextFiles,
    pub permission: Option<crate::PermCheck>,
    pub ask_tx: Option<mpsc::Sender<crate::permission::ask::AskRequest>>,
    pub ask_rx: Option<mpsc::Receiver<crate::permission::ask::AskRequest>>,
    pub sandbox: Sandbox,
    pub status_signals: Option<crate::extras::status_signals::StatusSignals>,

    pub ui: UiContext,
    pub agent_run: AgentRunState,
    pub chain: ChainState,
    pub btw: BtwState,

    pub user_tx: mpsc::Sender<UserEvent>,
    pub user_rx: mpsc::Receiver<UserEvent>,
    pub running: Arc<AtomicBool>,
    pub event_handle: Option<std::thread::JoinHandle<()>>,
    pub prebuild_rx: Option<mpsc::Receiver<PrebuildPayload>>,
    pub btw_rx: mpsc::Receiver<crate::event::BtwEvent>,

    #[cfg(feature = "mcp")]
    pub mcp_manager: Option<McpClientManager>,
    #[cfg(feature = "loop")]
    pub loop_state: Option<LoopState>,
    #[cfg(feature = "loop")]
    pub loop_label: Option<String>,
    #[cfg(feature = "git-worktree")]
    pub wt_return_path: Option<(String, String, String, bool)>,
    #[cfg(feature = "advisor")]
    pub handoff_rx: Option<advisor::HandoffReceiver>,
}

impl<'a> App<'a> {
    #[allow(clippy::too_many_arguments)]
    pub async fn new(
        client: AnyClient,
        agent: Option<AnyAgent>,
        cli: &'a Cli,
        cfg: &'a Config,
        session: &'a mut Session,
        context: &'a mut ContextFiles,
        permission: Option<crate::PermCheck>,
        ask_tx: Option<mpsc::Sender<crate::permission::ask::AskRequest>>,
        ask_rx: Option<mpsc::Receiver<crate::permission::ask::AskRequest>>,
        sandbox: Sandbox,
        status_signals: Option<crate::extras::status_signals::StatusSignals>,
        #[cfg(feature = "advisor")] handoff_rx: Option<advisor::HandoffReceiver>,
    ) -> anyhow::Result<Self> {
        let _guard = TerminalGuard::new()?;
        std::mem::forget(_guard);

        session.show_cost_always = cfg.resolve_show_cost_always();
        statusline::init(cfg);

        session.refresh_git_branch();
        if statusline::needs_git_status() {
            session.refresh_git_status();
        }

        let mut renderer = Renderer::new()?;
        renderer.set_statusline_height(statusline::line_count());
        renderer.set_monochrome(cli.no_color);
        renderer.set_chat_margin(cfg.resolve_chat_left_margin());
        if let Some(ref theme_name) = context.current_theme_name {
            if let Some(content) = context.themes.get(theme_name.as_str()) {
                crate::context::themes::apply(content, &mut renderer);
            }
        } else if let Some(colors) = &cfg.colors {
            let chat_bg = colors.chat_background.as_deref().and_then(parse_color);
            let input_bg = colors.input_background.as_deref().and_then(parse_color);
            let status_bg = colors.status_background.as_deref().and_then(parse_color);
            if matches!(colors.scheme_type, crate::config::SchemeType::Ansi) {
                renderer.set_background_colors(
                    chat_bg.map(to_ansi_256),
                    input_bg.map(to_ansi_256),
                    status_bg.map(to_ansi_256),
                );
            } else {
                renderer.set_background_colors(chat_bg, input_bg, status_bg);
            }
        }

        let mut input = InputEditor::new();
        input.set_monochrome(cli.no_color);
        input.set_prompt_names(context.prompts.keys().cloned().collect());
        input.set_theme_names(context.themes.keys().cloned().collect());
        if let Some(editor) = &cfg.editor {
            input.set_editor(editor.clone());
        }
        input.set_quick_model_names(crate::config::quick_models_map(cfg).into_keys().collect());
        {
            let mut providers: Vec<String> =
                ["anthropic", "openai", "gemini", "openrouter", "ollama"]
                    .iter()
                    .map(|s| s.to_string())
                    .collect();
            providers.extend(cfg.custom_providers_map().keys().cloned());
            input.set_provider_names(providers);
        }
        input.load_global_history();

        let reasoning_enabled = true;
        session.reasoning_enabled = reasoning_enabled;
        session.overhead_tokens =
            crate::agent::builder::estimate_overhead(context, reasoning_enabled);

        let (btw, btw_rx) = BtwState::new();

        let (user_tx, user_rx) = mpsc::channel::<UserEvent>(64);
        let running = Arc::new(AtomicBool::new(true));
        let event_handle = Some(spawn_event_thread(user_tx.clone(), running.clone()));

        Ok(Self {
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
            ui: UiContext {
                renderer,
                input,
                show_reasoning: cfg.resolve_show_reasoning(),
                reasoning_enabled,
                todo_tools_enabled: false,
                last_branch_check: std::time::Instant::now(),
            },
            agent_run: AgentRunState::default(),
            chain: ChainState::default(),
            btw,
            user_tx,
            user_rx,
            running,
            event_handle,
            prebuild_rx: None,
            btw_rx,
            #[cfg(feature = "mcp")]
            mcp_manager: None,
            #[cfg(feature = "loop")]
            loop_state: None,
            #[cfg(feature = "loop")]
            loop_label: None,
            #[cfg(feature = "git-worktree")]
            wt_return_path: None,
            #[cfg(feature = "advisor")]
            handoff_rx,
        })
    }

    // ── accessors ──

    pub fn perm_mode(&self) -> Option<String> {
        self.permission.as_ref().map(|p| {
            p.lock()
                .unwrap_or_else(|e| e.into_inner())
                .mode()
                .to_string()
        })
    }

    pub fn is_running(&self) -> bool {
        self.agent_run.is_running
    }

    #[cfg(feature = "loop")]
    pub fn loop_label(&self) -> Option<&str> {
        self.loop_label.as_deref()
    }
    #[cfg(not(feature = "loop"))]
    pub fn loop_label(&self) -> Option<&str> {
        None
    }

    pub fn refresh(&mut self) -> io::Result<()> {
        let loop_label = self.loop_label().map(|s| s.to_string());
        let prompt_name = self.context.current_prompt_name.clone();
        let perm_mode = self.perm_mode();
        let chain_label = self.chain.label_msg.clone();
        let btw_cost = self.btw.total_cost;
        let btw_in = self.btw.total_in;
        let btw_out = self.btw.total_out;
        let is_running = self.agent_run.is_running;
        refresh_display(
            &mut self.ui.renderer,
            &mut self.ui.input,
            self.session,
            is_running,
            loop_label.as_deref(),
            prompt_name.as_deref(),
            perm_mode.as_deref(),
            chain_label.as_deref(),
            btw_cost,
            btw_in,
            btw_out,
        )
    }

    // ── agent lifecycle ──

    /// Rebuild the agent. Centralizes the agent construction logic used by
    /// start_main_run, slash commands, and mid-turn compaction.
    pub async fn rebuild_agent(&mut self) {
        #[cfg(feature = "mcp")]
        let mcp_ref = super::ensure_mcp_manager(&mut self.mcp_manager, self.cfg).await;
        let model = self.client.completion_model(self.session.model.to_string());
        let temperature =
            crate::config::resolve_temperature(self.cli, self.cfg, &self.session.model);
        let extra_body = crate::config::resolve_extra_body(self.cfg, &self.session.model);
        self.agent = Some(
            crate::provider::build_agent(
                model,
                self.cli,
                self.cfg,
                self.context,
                self.permission.clone(),
                self.ask_tx.clone(),
                self.sandbox.clone(),
                self.ui.reasoning_enabled,
                temperature,
                extra_body,
                #[cfg(feature = "mcp")]
                mcp_ref,
            )
            .await,
        );
    }
}
