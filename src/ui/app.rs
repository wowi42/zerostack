use std::collections::VecDeque;
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use compact_str::CompactString;
use crossterm::ExecutableCommand;
use tokio::sync::mpsc;

use crate::cli::Cli;
use crate::config;
use crate::config::Config;
use crate::context::ContextFiles;
use crate::event::{AgentEvent, BtwEvent, UserEvent};
use crate::permission;
use crate::provider::{AnyAgent, AnyClient};
use crate::sandbox::Sandbox;
use crate::session::{MessageRole, Session};

use super::events::sanitize_output;
use super::input::InputEditor;
use super::renderer::{self, Renderer};
use super::statusline;
use super::terminal::TerminalGuard;
use super::{
    PrebuildPayload, copy_to_clipboard, parse_color, refresh_display, spawn_event_thread,
    to_ansi_256,
};

#[cfg(feature = "advisor")]
use crate::extras::advisor;
#[cfg(feature = "git-worktree")]
use crate::extras::git_worktree;
#[cfg(feature = "loop")]
use crate::extras::r#loop::LoopState;
#[cfg(feature = "mcp")]
use crate::extras::mcp::McpClientManager;

#[cfg(feature = "mcp")]
use super::ensure_mcp_manager;

use super::{C_AGENT, C_BTW, C_ERROR, C_PERM, C_TOOL, MID_TURN_CONTINUE_PROMPT};

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

    _guard: TerminalGuard,

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
        let guard = TerminalGuard::new()?;

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
            if matches!(colors.scheme_type, config::SchemeType::Ansi) {
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
        input.set_quick_model_names(config::quick_models_map(cfg).into_keys().collect());
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
            _guard: guard,
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

    pub async fn rebuild_agent(&mut self) {
        #[cfg(feature = "mcp")]
        let mcp_ref = ensure_mcp_manager(&mut self.mcp_manager, self.cfg).await;
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

    // ── event loop ──

    pub async fn run(&mut self, auto_trigger_msg: Option<String>) -> anyhow::Result<()> {
        use super::SubmitAction;
        use super::apply_current_prompt_mode;
        use super::classify_submission;
        use super::event_handler::{ensure_agent, handle_agent_event};
        use super::events::{render_session, show_welcome};
        use super::mid_turn_compact_and_respawn;
        use super::permission_handler::handle_permission_request;
        use super::resolve_prebuild;
        use super::slash::handle_compress;
        use super::slash::{apply_prompt_model, handle_slash, warm_model_cache};
        #[cfg(feature = "git-worktree")]
        use super::spawn_merge_agent;
        use super::start_main_run;
        use super::stop_turn_context_exhausted;
        use crossterm::event::{KeyCode, KeyModifiers};

        const TURN_TRACE_MAX: usize = 64;

        render_session(
            &mut self.ui.renderer,
            self.session,
            self.cli,
            self.cfg,
            self.context,
        )?;
        let marker_path = crate::session::storage::data_dir().join("shown_welcome_msg");
        if self.cfg.resolve_always_show_welcome() || !marker_path.exists() {
            show_welcome(&mut self.ui.renderer)?;
            if !self.cfg.resolve_always_show_welcome() {
                if let Some(dir) = marker_path.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                let _ = std::fs::write(&marker_path, "");
            }
        }
        self.refresh()?;

        // pre-warm model cache
        {
            let provider = self.session.provider.to_string();
            let is_custom = self.cfg.custom_providers_map().contains_key(&provider);
            let ids =
                warm_model_cache(&provider, is_custom, &self.client, self.cli, self.cfg).await;
            self.ui.input.set_live_model_names(ids);
        }

        #[cfg(feature = "git-worktree")]
        if let Some(name) = &self.cli.worktree {
            let wt_base_dir = self.cli.resolve_wt_base_dir(self.cfg);
            match git_worktree::create(name, wt_base_dir.as_deref()) {
                Ok((path, _info)) => {
                    std::env::set_current_dir(&path).ok();
                    self.session.working_dir =
                        compact_str::CompactString::new(path.to_string_lossy());
                    self.context.reload();
                    apply_current_prompt_mode(self.context, &self.permission);
                    self.rebuild_agent().await;
                    let _ = render_session(
                        &mut self.ui.renderer,
                        self.session,
                        self.cli,
                        self.cfg,
                        self.context,
                    );
                }
                Err(e) => {
                    let _ = self
                        .ui
                        .renderer
                        .write_line(&format!("worktree failed: {}", e), C_ERROR);
                }
            }
        }
        #[cfg(feature = "git-worktree")]
        if self.cli.parallel {
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let name = ts.to_string();
            let wt_base_dir = self.cli.resolve_wt_base_dir(self.cfg);
            match git_worktree::create(&name, wt_base_dir.as_deref()) {
                Ok((path, _info)) => {
                    std::env::set_current_dir(&path).ok();
                    self.session.working_dir =
                        compact_str::CompactString::new(path.to_string_lossy());
                    self.context.reload();
                    apply_current_prompt_mode(self.context, &self.permission);
                    self.rebuild_agent().await;
                    let _ = render_session(
                        &mut self.ui.renderer,
                        self.session,
                        self.cli,
                        self.cfg,
                        self.context,
                    );
                }
                Err(e) => {
                    let _ = self
                        .ui
                        .renderer
                        .write_line(&format!("worktree failed: {}", e), C_ERROR);
                }
            }
        }

        if let Some(ref trigger_msg) = auto_trigger_msg {
            for line in trigger_msg.lines() {
                let safe_line = sanitize_output(line);
                self.ui
                    .renderer
                    .write_line(&format!("> {}", safe_line), crossterm::style::Color::Green)?;
            }
            self.ui
                .renderer
                .write_line("", crossterm::style::Color::White)?;

            #[cfg(feature = "mcp")]
            let mcp_ref = ensure_mcp_manager(&mut self.mcp_manager, self.cfg).await;
            ensure_agent(
                &mut self.agent,
                &self.client,
                self.session,
                self.cli,
                self.cfg,
                self.context,
                &self.permission,
                &self.ask_tx,
                &self.sandbox,
                self.ui.reasoning_enabled,
                #[cfg(feature = "mcp")]
                mcp_ref,
            )
            .await;
            let history = crate::agent::runner::convert_history(self.session);
            let runner = self
                .agent
                .as_ref()
                .unwrap()
                .clone()
                .spawn_runner(
                    trigger_msg.to_string(),
                    history,
                    self.cfg.retry.clone(),
                    #[cfg(feature = "hooks")]
                    None,
                )
                .await;
            self.agent_run.agent_rx = Some(runner.event_rx);
            self.agent_run.main_abort = Some(runner.abort_handle);
            self.agent_run.is_running = true;
            if let Some(ss) = self.status_signals.as_ref() {
                ss.send_start();
            }
            self.session.add_message(MessageRole::User, trigger_msg);
            #[cfg(feature = "advisor")]
            crate::extras::advisor::set_session_messages(self.session.messages.clone());
        }

        // Prebuild the agent on a background task.
        let (prebuild_tx, prebuild_rx_raw) = mpsc::channel::<PrebuildPayload>(1);
        self.prebuild_rx = Some(prebuild_rx_raw);
        if auto_trigger_msg.is_none() && self.agent.is_none() {
            let client_clone = self.client.clone();
            let session_model = self.session.model.to_string();
            let cli_clone = self.cli.clone();
            let cfg_clone = self.cfg.clone();
            let context_clone = self.context.clone();
            let permission_clone = self.permission.clone();
            let ask_tx_clone = self.ask_tx.clone();
            let sandbox_clone = self.sandbox.clone();
            let reasoning_enabled = self.ui.reasoning_enabled;
            tokio::spawn(async move {
                #[cfg(feature = "mcp")]
                let mcp = if let Some(ref servers) = cfg_clone.mcp_servers {
                    if !servers.is_empty() {
                        Some(McpClientManager::connect_all(servers).await)
                    } else {
                        None
                    }
                } else {
                    None
                };

                let model = client_clone.completion_model(session_model.clone());
                let temperature =
                    crate::config::resolve_temperature(&cli_clone, &cfg_clone, &session_model);
                let extra_body = crate::config::resolve_extra_body(&cfg_clone, &session_model);
                let a = crate::provider::build_agent(
                    model,
                    &cli_clone,
                    &cfg_clone,
                    &context_clone,
                    permission_clone,
                    ask_tx_clone,
                    sandbox_clone,
                    reasoning_enabled,
                    temperature,
                    extra_body,
                    #[cfg(feature = "mcp")]
                    mcp.as_ref(),
                )
                .await;

                #[cfg(feature = "mcp")]
                let _ = prebuild_tx.send((a, mcp)).await;
                #[cfg(not(feature = "mcp"))]
                let _ = prebuild_tx.send(a).await;
            });
        }

        // ── main event loop ──

        loop {
            self.session.reasoning_enabled = self.ui.reasoning_enabled;
            if self.ui.last_branch_check.elapsed() >= std::time::Duration::from_secs(1) {
                self.session.refresh_git_branch();
                if statusline::needs_git_status() {
                    self.session.refresh_git_status();
                }
                self.ui.last_branch_check = std::time::Instant::now();
            }
            tokio::select! {
                Some(ev) = self.user_rx.recv() => {
                    match ev {
                        UserEvent::Resize => {
                            self.ui.renderer.resize();
                            self.refresh()?;
                            continue;
                        }
                        UserEvent::ScrollUp => {
                            if !self.ui.renderer.input_scroll_up() {
                                self.ui.renderer.scroll_line_up();
                            }
                            self.refresh()?;
                            continue;
                        }
                        UserEvent::ScrollDown => {
                            if self.ui.renderer.is_scrolling() {
                                self.ui.renderer.scroll_line_down();
                            } else {
                                self.ui.renderer.input_scroll_down();
                            }
                            self.refresh()?;
                            continue;
                        }
                        UserEvent::MouseDown { row, col } => {
                            if let Some(pos) = self.ui.renderer.input_cursor_for_click(row, col, &self.ui.input.buffer) {
                                self.ui.input.set_cursor(pos);
                                self.refresh()?;
                            } else if row < self.ui.renderer.visible_lines() as u16
                                && let Some(idx) = self.ui.renderer.buffer_line_at_row(row) {
                                    if let Some(url) = self.ui.renderer.link_url_at(idx, col) {
                                        renderer::open_url(url);
                                    } else {
                                        self.ui.renderer.selection_active = true;
                                        self.ui.renderer.selection_start = Some(idx);
                                        self.ui.renderer.selection_end = Some(idx);
                                        self.refresh()?;
                                    }
                                }
                            continue;
                        }
                        UserEvent::MouseDrag { row, col: _ } => {
                            if self.ui.renderer.selection_active
                                && let Some(idx) = self.ui.renderer.buffer_line_at_row(row) {
                                    self.ui.renderer.selection_end = Some(idx);
                                    self.refresh()?;
                                }
                            continue;
                        }
                        UserEvent::MouseUp { row, col: _ } => {
                            if self.ui.renderer.selection_active {
                                if let Some(idx) = self.ui.renderer.buffer_line_at_row(row) {
                                    self.ui.renderer.selection_end = Some(idx);
                                }
                                if let Some(text) = self.ui.renderer.selected_text() {
                                    copy_to_clipboard(&text);
                                }
                                self.ui.renderer.clear_selection();
                                self.refresh()?;
                            }
                            continue;
                        }
                        UserEvent::Paste(data) => {
                            self.ui.input.handle_paste(data);
                            self.refresh()?;
                            continue;
                        }
                        #[cfg(feature = "mcp")]
                        UserEvent::McpLoginDone { server, error } => {
                            if let Some(err) = error {
                                self.ui.renderer.write_line(&format!("login failed for '{}': {}", server, err), C_ERROR)?;
                            } else {
                                let server = server.to_string();
                                let server_cfg = self.cfg.mcp_servers.as_ref().and_then(|m| m.get(&server).cloned());
                                match (self.mcp_manager.as_mut(), server_cfg) {
                                    (Some(mgr), Some(scfg)) => {
                                        match mgr.reconnect(&server, &scfg).await {
                                            Ok(()) => {
                                                self.rebuild_agent().await;
                                                self.ui.renderer.write_line(&format!("authorized and connected '{}'", server), C_AGENT)?;
                                            }
                                            Err(err) => {
                                                self.ui.renderer.write_line(&format!("authorized '{}' but reconnect failed: {}", server, err), C_ERROR)?;
                                            }
                                        }
                                    }
                                    _ => {
                                        self.ui.renderer.write_line(&format!("authorized '{}' (will connect on next start)", server), C_AGENT)?;
                                    }
                                }
                            }
                            self.refresh()?;
                            continue;
                        }
                        UserEvent::Key(key) => {
                            let is_ctrl_c = key.code == KeyCode::Char('c')
                                && key.modifiers.contains(KeyModifiers::CONTROL);
                            let is_ctrl_d = key.code == KeyCode::Char('d')
                                && key.modifiers.contains(KeyModifiers::CONTROL);
                            if is_ctrl_c || is_ctrl_d {
                                if self.btw.inflight > 0 {
                                    for (_, h) in self.btw.abort.drain(..) {
                                        h.abort();
                                    }
                                    self.btw.inflight = 0;
                                    self.ui.renderer.write_line("btw cancelled", C_ERROR)?;
                                    self.refresh()?;
                                } else if self.agent_run.is_running {
                                    if let Some(h) = self.agent_run.main_abort.take() {
                                        h.abort();
                                    }
                                    self.sandbox.kill_active();
                                    self.agent_run.is_running = false;
                                    if let Some(ss) = self.status_signals.as_ref() {
                                        ss.send_stop();
                                    }
                                    self.agent_run.agent_rx = None;
                                    self.agent_run.turn_trace.clear();
                                    self.agent_run.awaiting_compaction_relief = false;
                                    self.agent_run.pending_inputs.clear();
                                    #[cfg(feature = "loop")]
                                    if let Some(ref mut ls) = self.loop_state {
                                        ls.active = false;
                                        self.loop_label = None;
                                    }
                                    if !self.ui.input.buffer.is_empty() {
                                        self.ui.input.clear_buffer();
                                    }
                                    if let Some(restore_name) = self.chain.dot_prompt_restore.take() {
                                        self.context.current_prompt = self.context.prompts.get(&restore_name).cloned();
                                        self.context.current_prompt_name = if self.context.current_prompt.is_some() {
                                            Some(restore_name)
                                        } else {
                                            None
                                        };
                                        if let Some(perm) = &self.permission {
                                            let mut guard = perm.lock().unwrap_or_else(|e| e.into_inner());
                                            guard.restore_user_mode();
                                        }
                                    }
                                    self.ui.renderer.write_line(
                                        "interrupted (changes may be partial; review with git diff)",
                                        C_ERROR,
                                    )?;
                                    self.refresh()?;
                                } else {
                                    break;
                                }
                                continue;
                            }

                            if self.ui.renderer.selection_active && key.code == KeyCode::Char('y') {
                                if let Some(text) = self.ui.renderer.selected_text() {
                                    copy_to_clipboard(&text);
                                    self.ui.renderer.write_line("copied selection", crossterm::style::Color::Green)?;
                                }
                                self.ui.renderer.clear_selection();
                                self.refresh()?;
                                continue;
                            }
                            if self.ui.renderer.selection_active && key.code == KeyCode::Esc {
                                self.ui.renderer.clear_selection();
                                self.refresh()?;
                                continue;
                            }
                            let ctrl_r = key.code == KeyCode::Char('r')
                                && key.modifiers.contains(KeyModifiers::CONTROL);
                            if ctrl_r {
                                self.ui.show_reasoning = !self.ui.show_reasoning;
                                self.ui.renderer.write_line(
                                    &format!("reasoning visibility: {}", if self.ui.show_reasoning { "on" } else { "off" }),
                                    crossterm::style::Color::White,
                                )?;
                                self.refresh()?;
                                continue;
                            }

                            match key.code {
                                KeyCode::PageUp => {
                                    self.ui.renderer.scroll_page_up();
                                    self.refresh()?;
                                    continue;
                                }
                                KeyCode::PageDown => {
                                    self.ui.renderer.scroll_page_down();
                                    self.refresh()?;
                                    continue;
                                }
                                KeyCode::Home => {
                                    self.ui.renderer.scroll_to_top();
                                    self.refresh()?;
                                    continue;
                                }
                                KeyCode::End => {
                                    self.ui.renderer.scroll_to_bottom()?;
                                    self.refresh()?;
                                    continue;
                                }
                                _ => {}
                            }

                            if self.ui.input.picker.as_ref().is_some_and(|p| p.active())
                                && self.ui.input.handle_picker_key(key) {
                                    if let Some(super::RewindOutcome::Confirmed(idx)) =
                                        self.ui.input.take_rewind_outcome()
                                    {
                                        let text =
                                            self.session.messages.get(idx).map(|m| m.content.to_string());
                                        if self.session.rewind_to(idx) > 0 {
                                            if let Some(text) = text {
                                                self.ui.input.load_text(&text);
                                            }
                                            if !self.cli.no_session {
                                                let _ = crate::session::storage::save_session(self.session);
                                            }
                                            render_session(&mut self.ui.renderer, self.session, self.cli, self.cfg, self.context)?;
                                            self.ui.renderer.write_line(
                                                "rewound; /redo to restore",
                                                crossterm::style::Color::Green,
                                            )?;
                                        }
                                    }
                                    self.refresh()?;
                                    continue;
                                }

                            if key.code == KeyCode::Char('g') && key.modifiers.contains(KeyModifiers::CONTROL) {
                                if let Some(h) = self.event_handle.take() {
                                    self.running.store(false, Ordering::Relaxed);
                                    let _ = h.join();
                                }
                                self.ui.input.open_in_editor();
                                self.running = Arc::new(AtomicBool::new(true));
                                let (new_tx, new_rx) = mpsc::channel(64);
                                self.user_tx = new_tx;
                                self.user_rx = new_rx;
                                self.event_handle = Some(spawn_event_thread(self.user_tx.clone(), self.running.clone()));
                                self.refresh()?;
                                continue;
                            }

                            if key.code == KeyCode::Char('h') && key.modifiers.contains(KeyModifiers::CONTROL) {
                                if std::process::Command::new("lazygit")
                                    .arg("--version")
                                    .output()
                                    .is_err()
                                {
                                    self.ui.renderer.write_line(
                                        "warning: lazygit not found — install it (https://github.com/jesseduffield/lazygit)",
                                        C_ERROR,
                                    )?;
                                    self.refresh()?;
                                    continue;
                                }
                                if let Some(h) = self.event_handle.take() {
                                    self.running.store(false, Ordering::Relaxed);
                                    let _ = h.join();
                                }
                                let _ = crossterm::terminal::disable_raw_mode();
                                let mut stdout = std::io::stdout();
                                let _ = stdout.execute(crossterm::event::DisableMouseCapture);
                                let _ = stdout.execute(crossterm::terminal::LeaveAlternateScreen);
                                let _ = std::io::Write::flush(&mut stdout);
                                let _ = std::process::Command::new("lazygit").status();
                                let _ = stdout.execute(crossterm::terminal::EnterAlternateScreen);
                                let _ = stdout.execute(crossterm::terminal::Clear(crossterm::terminal::ClearType::All));
                                let _ = stdout.execute(crossterm::event::EnableMouseCapture);
                                let _ = crossterm::terminal::enable_raw_mode();
                                self.running = Arc::new(AtomicBool::new(true));
                                let (new_tx, new_rx) = mpsc::channel(64);
                                self.user_tx = new_tx;
                                self.user_rx = new_rx;
                                self.event_handle = Some(spawn_event_thread(self.user_tx.clone(), self.running.clone()));
                                self.refresh()?;
                                continue;
                            }

                            // Chain prompt active: intercept Y/N/B keystrokes
                            if self.ui.renderer.chain_prompt.is_some() && !self.ui.renderer.chain_but_mode {
                                match key.code {
                                    KeyCode::Char('y') | KeyCode::Char('Y') => {
                                        self.ui.renderer.chain_prompt = None;
                                        if let Some(phase) = self.chain.pending.take() {
                                            self.chain.label_msg = None;
                                            let next_name = phase.next_prompt_name();
                                            if let Some(content) = self.context.prompts.get(next_name).cloned() {
                                                let (mode_directive_str, clean_content) =
                                                    permission::parse_prompt_mode(&content);
                                                let mode_directive = mode_directive_str.map(|s| s.to_string());
                                                self.context.current_prompt = Some(if mode_directive.is_some() {
                                                    clean_content.to_string()
                                                } else {
                                                    content
                                                });
                                                self.context.current_prompt_name = Some(next_name.to_string());
                                                if let Some(ref mode_str) = mode_directive {
                                                    if mode_str == "last_user_mode"
                                                        && let Some(perm) = &self.permission
                                                    {
                                                        let mut guard = perm.lock().unwrap_or_else(|e| e.into_inner());
                                                        guard.restore_user_mode();
                                                    } else if let Some(mode) =
                                                        permission::SecurityMode::from_str(mode_str)
                                                        && let Some(perm) = &self.permission
                                                    {
                                                        let mut guard = perm.lock().unwrap_or_else(|e| e.into_inner());
                                                        guard.set_prompt_mode(mode);
                                                    }
                                                }
                                            }
                                            #[cfg(feature = "mcp")]
                                            let mcp_ref = ensure_mcp_manager(&mut self.mcp_manager, self.cfg).await;
                                            apply_prompt_model(
                                                next_name,
                                                self.cfg,
                                                self.cli,
                                                &mut self.client,
                                                self.session,
                                                &mut self.agent,
                                                self.context,
                                                &self.permission,
                                                &self.ask_tx,
                                                &self.sandbox,
                                                self.ui.reasoning_enabled,
                                                &mut self.ui.renderer,
                                                #[cfg(feature = "mcp")]
                                                mcp_ref,
                                            )
                                            .await;
                                            let msg = phase.transition_message().to_string();
                                            for line in msg.lines() {
                                                self.ui.renderer.write_line(
                                                    &format!("> {}", sanitize_output(line)),
                                                    crossterm::style::Color::Green,
                                                )?;
                                            }
                                            self.ui.renderer.write_line("", crossterm::style::Color::White)?;
                                            self.session.add_message(MessageRole::User, &msg);
                                            self.agent = None;
                                            start_main_run(
                                                &msg, &mut self.agent, &self.client, self.session, self.cli,
                                                self.cfg, self.context, &self.permission, &self.ask_tx, &self.sandbox,
                                                self.ui.reasoning_enabled, &mut self.agent_run.agent_rx,
                                                &mut self.agent_run.main_abort, &mut self.agent_run.is_running,
                                                &self.status_signals,
                                                #[cfg(feature = "mcp")] &mut self.mcp_manager,
                                                &mut self.prebuild_rx,
                                                &mut self.agent_run.pending_send,
                                            ).await;
                                        }
                                        self.refresh()?;
                                        continue;
                                    }
                                    KeyCode::Char('n') | KeyCode::Char('N') => {
                                        self.ui.renderer.chain_prompt = None;
                                        self.chain.pending = None;
                                        self.chain.label_msg = None;
                                        self.ui.renderer.write_line(
                                            "chain declined — won't ask again this session",
                                            C_AGENT,
                                        )?;
                                        if let Some(ref name) = self.context.current_prompt_name
                                            && !self.context.chain_declined.contains(name)
                                        {
                                            self.context.chain_declined.push(name.clone());
                                        }
                                        self.refresh()?;
                                        continue;
                                    }
                                    KeyCode::Char('b') | KeyCode::Char('B') => {
                                        self.ui.renderer.chain_but_mode = true;
                                        self.ui.renderer.chain_prompt = None;
                                        self.ui.input.clear_buffer();
                                        self.chain.label_msg = self.chain.pending.map(|p| p.chain_label().to_string());
                                        self.refresh()?;
                                        continue;
                                    }
                                    _ => {
                                        continue;
                                    }
                                }
                            }
                            // Chain but mode: Esc cancels back to ask
                            if self.ui.renderer.chain_but_mode && key.code == KeyCode::Esc {
                                self.ui.renderer.chain_but_mode = false;
                                if let Some(phase) = self.chain.pending {
                                    self.ui.renderer.chain_prompt = Some(renderer::ChainPrompt {
                                        question: compact_str::CompactString::from(phase.chain_label()),
                                    });
                                    self.chain.label_msg = Some(phase.chain_label().to_string());
                                }
                                self.ui.input.clear_buffer();
                                self.refresh()?;
                                continue;
                            }

                            if let Some(mut text) = self.ui.input.handle_key(key) {
                                #[cfg(feature = "loop")]
                                if self.loop_state.as_ref().is_some_and(|ls| ls.active) && !text.starts_with('/') {
                                    self.ui.renderer.write_line("loop active: /loop stop to cancel", C_ERROR)?;
                                    self.refresh()?;
                                    continue;
                                }
                                if self.ui.renderer.is_scrolling() {
                                    self.ui.renderer.scroll_to_bottom()?;
                                }
                                // Chain-of-prompts: handle text submission after B (but) mode
                                if !self.agent_run.is_running
                                    && let Some(phase) = self.chain.pending.take()
                                {
                                    self.chain.label_msg = None;
                                    self.ui.renderer.chain_but_mode = false;
                                    let trimmed = text.trim().to_string();
                                    if trimmed.is_empty() {
                                        self.chain.pending = Some(phase);
                                        self.chain.label_msg = Some(phase.chain_label().to_string());
                                        self.ui.renderer.chain_prompt = Some(renderer::ChainPrompt {
                                            question: compact_str::CompactString::from(phase.chain_label()),
                                        });
                                        self.refresh()?;
                                        continue;
                                    }
                                    let next_name = phase.next_prompt_name();
                                    if let Some(content) = self.context.prompts.get(next_name).cloned() {
                                        let (mode_directive_str, clean_content) =
                                            permission::parse_prompt_mode(&content);
                                        let mode_directive = mode_directive_str.map(|s| s.to_string());
                                        self.context.current_prompt = Some(if mode_directive.is_some() {
                                            clean_content.to_string()
                                        } else {
                                            content
                                        });
                                        self.context.current_prompt_name = Some(next_name.to_string());
                                        if let Some(ref mode_str) = mode_directive {
                                            if mode_str == "last_user_mode"
                                                && let Some(perm) = &self.permission
                                            {
                                                let mut guard = perm.lock().unwrap_or_else(|e| e.into_inner());
                                                guard.restore_user_mode();
                                            } else if let Some(mode) =
                                                permission::SecurityMode::from_str(mode_str)
                                                && let Some(perm) = &self.permission
                                            {
                                                let mut guard = perm.lock().unwrap_or_else(|e| e.into_inner());
                                                guard.set_prompt_mode(mode);
                                            }
                                        }
                                    }
                                    #[cfg(feature = "mcp")]
                                    let mcp_ref = ensure_mcp_manager(&mut self.mcp_manager, self.cfg).await;
                                    apply_prompt_model(
                                        next_name,
                                        self.cfg,
                                        self.cli,
                                        &mut self.client,
                                        self.session,
                                        &mut self.agent,
                                        self.context,
                                        &self.permission,
                                        &self.ask_tx,
                                        &self.sandbox,
                                        self.ui.reasoning_enabled,
                                        &mut self.ui.renderer,
                                        #[cfg(feature = "mcp")]
                                        mcp_ref,
                                    )
                                    .await;
                                    let base_msg = phase.transition_message().to_string();
                                    let msg = format!("{}\n\nAdditional instructions: {}", base_msg, trimmed);
                                    for line in msg.lines() {
                                        self.ui.renderer.write_line(
                                            &format!("> {}", sanitize_output(line)),
                                            crossterm::style::Color::Green,
                                        )?;
                                    }
                                    self.ui.renderer.write_line("", crossterm::style::Color::White)?;
                                    self.session.add_message(MessageRole::User, &msg);
                                    self.agent = None;
                                    start_main_run(
                                        &msg, &mut self.agent, &self.client, self.session, self.cli,
                                        self.cfg, self.context, &self.permission, &self.ask_tx, &self.sandbox,
                                        self.ui.reasoning_enabled, &mut self.agent_run.agent_rx,
                                        &mut self.agent_run.main_abort, &mut self.agent_run.is_running,
                                        &self.status_signals,
                                        #[cfg(feature = "mcp")] &mut self.mcp_manager,
                                        &mut self.prebuild_rx,
                                        &mut self.agent_run.pending_send,
                                    ).await;
                                    self.refresh()?;
                                    continue;
                                }
                                match classify_submission(self.agent_run.is_running, &text) {
                                    SubmitAction::Run => {}
                                    SubmitAction::Ignore => {
                                        self.refresh()?;
                                        continue;
                                    }
                                    SubmitAction::RejectWhileRunning => {
                                        self.ui.renderer.write_line(
                                            "agent is running — wait for it to finish or press Ctrl-C before running a command",
                                            C_ERROR,
                                        )?;
                                        self.refresh()?;
                                        continue;
                                    }
                                    SubmitAction::Queue => {
                                        self.agent_run.pending_inputs.push_back(text.to_string());
                                        self.ui.renderer.write_line(&format!("queued: {}", sanitize_output(&text)), C_TOOL)?;
                                        self.refresh()?;
                                        continue;
                                    }
                                }
                                // Bypass-slot handlers
                                {
                                    let t = text.trim_start();
                                    if t == "/queue" || t.starts_with("/queue ") {
                                        let arg = t.strip_prefix("/queue").unwrap_or("").trim();
                                        match arg {
                                            "clear" => {
                                                let n = self.agent_run.pending_inputs.len();
                                                self.agent_run.pending_inputs.clear();
                                                self.ui.renderer.write_line(&format!("queue cleared ({} removed)", n), C_TOOL)?;
                                            }
                                            "pop" => match self.agent_run.pending_inputs.pop_back() {
                                                Some(x) => self.ui.renderer.write_line(&format!("unqueued: {}", sanitize_output(&x)), C_TOOL)?,
                                                None => self.ui.renderer.write_line("queue is empty", C_TOOL)?,
                                            },
                                            "" | "ls" | "list" => {
                                                if self.agent_run.pending_inputs.is_empty() {
                                                    self.ui.renderer.write_line("queue is empty", C_TOOL)?;
                                                } else {
                                                    self.ui.renderer.write_line(&format!("queued ({}):", self.agent_run.pending_inputs.len()), C_TOOL)?;
                                                    for (i, q) in self.agent_run.pending_inputs.iter().enumerate() {
                                                        self.ui.renderer.write_line(&format!("  {}. {}", i + 1, sanitize_output(q)), C_TOOL)?;
                                                    }
                                                }
                                            }
                                            _ => self.ui.renderer.write_line("usage: /queue [ls|clear|pop]", C_ERROR)?,
                                        }
                                        self.refresh()?;
                                        continue;
                                    }
                                }
                                // `/btw`
                                {
                                    let t = text.trim_start();
                                    if t == "/btw" || t.starts_with("/btw ") {
                                        for line in text.lines() {
                                            self.ui.renderer.write_line(&format!("> {}", sanitize_output(line)), crossterm::style::Color::Green)?;
                                        }
                                        self.ui.renderer.write_line("", crossterm::style::Color::White)?;
                                        let btw_text = t.strip_prefix("/btw").map(|s| s.trim()).unwrap_or("");
                                        if btw_text.is_empty() {
                                            self.ui.renderer.write_line("usage: /btw <message>", C_AGENT)?;
                                        } else {
                                            let id = self.btw.next_id;
                                            self.btw.next_id = self.btw.next_id.wrapping_add(1);
                                            let snapshot = crate::agent::runner::build_btw_snapshot(
                                                self.session, &self.agent_run.turn_trace, self.agent_run.is_running,
                                            );
                                            let model = self.client.completion_model(self.session.model.to_string());
                                            let temperature =
                                                crate::config::resolve_temperature(self.cli, self.cfg, &self.session.model);
                                            let extra_body =
                                                crate::config::resolve_extra_body(self.cfg, &self.session.model);
                                            let btw_agent = crate::provider::build_btw_agent(
                                                model, self.cli, self.cfg, self.context, &self.permission, &self.ask_tx, self.ui.reasoning_enabled, temperature, extra_body,
                                            );
                                            let runner = btw_agent.spawn_btw(
                                                btw_text.to_string(), snapshot, self.btw.tx.clone(), id, self.cfg.retry.clone(),
                                            );
                                            self.btw.abort.push((id, runner.abort_handle));
                                            self.btw.inflight += 1;
                                            self.ui.renderer.write_line(&format!("[btw #{}] thinking...", id), C_BTW)?;
                                        }
                                        self.refresh()?;
                                        continue;
                                    }
                                }
                                let mut is_dot_cmd = false;
                                if text.starts_with('.') {
                                    is_dot_cmd = true;
                                    let after_dot = text[1..].trim_start();

                                    for line in text.lines() {
                                        let safe_line = sanitize_output(line);
                                        self.ui.renderer.write_line(&format!("> {}", safe_line), crossterm::style::Color::Green)?;
                                    }
                                    self.ui.renderer.write_line("", crossterm::style::Color::White)?;

                                    if after_dot.is_empty() {
                                        self.ui.input.buffer = ".".into();
                                        self.ui.input.cursor = 1;
                                        self.ui.input.start_dot_picker();
                                    } else if let Some((prompt_name, msg)) = after_dot.split_once(char::is_whitespace) {
                                        let prompt_name = prompt_name.trim();
                                        let msg = msg.trim();
                                        if !prompt_name.is_empty() && self.context.prompts.contains_key(prompt_name) {
                                            self.chain.dot_prompt_restore = self.context.current_prompt_name.clone();
                                            if let Some(content) = self.context.prompts.get(prompt_name).cloned() {
                                                let (mode_directive_str, clean_content) = permission::parse_prompt_mode(&content);
                                                let mode_directive = mode_directive_str.map(|s| s.to_string());
                                                self.context.current_prompt = Some(if mode_directive.is_some() {
                                                    clean_content.to_string()
                                                } else {
                                                    content
                                                });
                                                self.context.current_prompt_name = Some(prompt_name.to_string());
                                                if let Some(ref mode_str) = mode_directive
                                                    && let Some(perm) = &self.permission {
                                                        let mut guard = perm.lock().unwrap_or_else(|e| e.into_inner());
                                                        if mode_str == "last_user_mode" {
                                                            guard.restore_user_mode();
                                                        } else if let Some(mode) = permission::SecurityMode::from_str(mode_str) {
                                                            guard.set_prompt_mode(mode);
                                                        }
                                                    }
                                            }
                                            #[cfg(feature = "mcp")]
                                            let mcp_ref = ensure_mcp_manager(&mut self.mcp_manager, self.cfg).await;
                                            apply_prompt_model(
                                                prompt_name,
                                                self.cfg,
                                                self.cli,
                                                &mut self.client,
                                                self.session,
                                                &mut self.agent,
                                                self.context,
                                                &self.permission,
                                                &self.ask_tx,
                                                &self.sandbox,
                                                self.ui.reasoning_enabled,
                                                &mut self.ui.renderer,
                                                #[cfg(feature = "mcp")]
                                                mcp_ref,
                                            )
                                            .await;
                                            text = msg.to_string().into();
                                            is_dot_cmd = false;
                                            self.agent = None;
                                        } else {
                                            self.ui.renderer.write_line(&format!("error: unknown prompt '{}'", prompt_name), C_ERROR)?;
                                        }
                                    } else {
                                        let prompt_name = after_dot.trim();
                                        if self.context.prompts.contains_key(prompt_name) {
                                            if let Some(content) = self.context.prompts.get(prompt_name).cloned() {
                                                let (mode_directive_str, clean_content) = permission::parse_prompt_mode(&content);
                                                let mode_directive = mode_directive_str.map(|s| s.to_string());
                                                self.context.current_prompt = Some(if mode_directive.is_some() {
                                                    clean_content.to_string()
                                                } else {
                                                    content
                                                });
                                                self.context.current_prompt_name = Some(prompt_name.to_string());
                                                if let Some(ref mode_str) = mode_directive
                                                    && let Some(perm) = &self.permission {
                                                        let mut guard = perm.lock().unwrap_or_else(|e| e.into_inner());
                                                        if mode_str == "last_user_mode" {
                                                            guard.restore_user_mode();
                                                        } else if let Some(mode) = permission::SecurityMode::from_str(mode_str) {
                                                            guard.set_prompt_mode(mode);
                                                        }
                                                    }
                                            }
                                            #[cfg(feature = "mcp")]
                                            let mcp_ref = ensure_mcp_manager(&mut self.mcp_manager, self.cfg).await;
                                            apply_prompt_model(
                                                prompt_name,
                                                self.cfg,
                                                self.cli,
                                                &mut self.client,
                                                self.session,
                                                &mut self.agent,
                                                self.context,
                                                &self.permission,
                                                &self.ask_tx,
                                                &self.sandbox,
                                                self.ui.reasoning_enabled,
                                                &mut self.ui.renderer,
                                                #[cfg(feature = "mcp")]
                                                mcp_ref,
                                            )
                                            .await;
                                            self.agent = None;
                                            self.ui.renderer.write_line(&format!("switched to prompt '{}'", prompt_name), C_AGENT)?;
                                            if !self.cli.no_session
                                                && let Err(e) = crate::session::storage::save_session(self.session)
                                            {
                                                self.ui.renderer.write_line(
                                                    &format!("warning: failed to save session: {}", e),
                                                    C_ERROR,
                                                )?;
                                            }
                                        } else {
                                            self.ui.renderer.write_line(&format!("error: unknown prompt '{}'", prompt_name), C_ERROR)?;
                                        }
                                    }
                                }
                                if !is_dot_cmd {
                                if text.starts_with('/') {
                                    for line in text.lines() {
                                        let safe_line = sanitize_output(line);
                                        self.ui.renderer.write_line(&format!("> {}", safe_line), crossterm::style::Color::Green)?;
                                    }
                                    self.ui.renderer.write_line("", crossterm::style::Color::White)?;
                                    #[cfg(feature = "mcp")]
                                    let mcp_ref = ensure_mcp_manager(&mut self.mcp_manager, self.cfg).await;
                                    let result = handle_slash(&text, &mut self.agent, &mut self.client, &mut self.ui.renderer, self.session, self.cli, self.cfg, self.context, &mut self.ui.show_reasoning, &mut self.ui.reasoning_enabled, &mut self.agent_run.is_running, &mut self.ui.input, &self.permission, &self.ask_tx, &mut self.ui.todo_tools_enabled, &self.sandbox, #[cfg(feature = "loop")] &mut self.loop_state, #[cfg(feature = "mcp")] mcp_ref).await;
                                    {
                                        let provider = self.session.provider.to_string();
                                        let is_custom = self.cfg.custom_providers_map().contains_key(&provider);
                                        let ids = warm_model_cache(&provider, is_custom, &self.client, self.cli, self.cfg).await;
                                        self.ui.input.set_live_model_names(ids);
                                    }
                                    match result {
                                    Err(e) if e.to_string().starts_with("DEFER_COMPRESS:") => {
                                        let err_msg = e.to_string();
                                        let instructions = err_msg.strip_prefix("DEFER_COMPRESS:").and_then(|s| {
                                            let s = s.trim();
                                            if s.is_empty() || s == "(none)" { None } else { Some(s.to_string()) }
                                        });
                                            #[cfg(feature = "mcp")]
                                            let mcp_ref = ensure_mcp_manager(&mut self.mcp_manager, self.cfg).await;
                                            let compress_result = handle_compress(
                                                instructions.as_deref(),
                                                false,
                                                &mut self.agent, &mut self.client, &mut self.ui.renderer, self.session, self.cli, self.cfg, self.context,
                                                self.ui.reasoning_enabled,
                                                &self.permission, &self.ask_tx, &self.sandbox,
                                                #[cfg(feature = "mcp")] mcp_ref,
                                            ).await;
                                            if let Err(e) = compress_result {
                                                self.ui.renderer.write_line(&format!("compress error: {}", e), C_ERROR)?;
                                            }
                                            let _ = crate::session::storage::save_session(self.session);
                                        }
                                        #[cfg(feature = "mcp")]
                                        Err(e) if e.to_string().starts_with(crate::ui::slash::settings::DEFER_MCP_LOGIN) => {
                                            let server = e.to_string()
                                                .strip_prefix(crate::ui::slash::settings::DEFER_MCP_LOGIN)
                                                .unwrap_or_default()
                                                .trim()
                                                .to_string();
                                            let resolved = self.cfg.mcp_servers.as_ref().and_then(|m| m.get(&server)).and_then(|s| {
                                                if let crate::extras::mcp::config::McpServerConfig::Url { url, oauth, .. } = s {
                                                    oauth.as_ref().and_then(|o| o.settings()).map(|set| (url.clone(), set))
                                                } else {
                                                    None
                                                }
                                            });
                                            match resolved {
                                                Some((url, settings)) => {
                                                    self.ui.renderer.write_line(&format!("starting OAuth login for '{}'...", server), C_AGENT)?;
                                                    match crate::extras::mcp::oauth::begin_login(&server, &url, &settings).await {
                                                        Ok(login) => {
                                                            copy_to_clipboard(&login.auth_url);
                                                            self.ui.renderer.write_line("open this URL to authorize (copied to clipboard):", C_AGENT)?;
                                                            self.ui.renderer.write_line(&login.auth_url, crossterm::style::Color::Cyan)?;
                                                            self.ui.renderer.write_line(
                                                                &format!("waiting for authorization on 127.0.0.1:{} in the background...", settings.redirect_port()),
                                                                crossterm::style::Color::DarkGrey,
                                                            )?;
                                                            let tx = self.user_tx.clone();
                                                            let sname = compact_str::CompactString::new(&server);
                                                            tokio::spawn(async move {
                                                                let error = login
                                                                    .wait_for_callback(std::time::Duration::from_secs(180))
                                                                    .await
                                                                    .err()
                                                                    .map(|e| compact_str::CompactString::new(e.to_string()));
                                                                let _ = tx.send(crate::event::UserEvent::McpLoginDone { server: sname, error }).await;
                                                            });
                                                        }
                                                        Err(err) => {
                                                            self.ui.renderer.write_line(&format!("login setup failed for '{}': {}", server, err), C_ERROR)?;
                                                        }
                                                    }
                                                }
                                                None => {
                                                    self.ui.renderer.write_line(&format!("cannot start login for '{}' (not an OAuth URL server)", server), C_ERROR)?;
                                                }
                                            }
                                        }
                                        #[cfg(feature = "git-worktree")]
                                        Err(e) if e.downcast_ref::<git_worktree::DeferredWorktreeAction>().is_some() => {
                                            let action = e.downcast_ref::<git_worktree::DeferredWorktreeAction>().unwrap();
                                            match action {
                                                git_worktree::DeferredWorktreeAction::Merge { branch, target, main_path, wt_path } => {
                                                    #[cfg(feature = "git-worktree")]
                                                    let force_flag = self.cli.resolve_wt_force(self.cfg);
                                                    #[cfg(not(feature = "git-worktree"))]
                                                    let force_flag = false;
                                                    spawn_merge_agent(
                                                        branch, target, main_path, wt_path,
                                                        force_flag,
                                                        self.session,
                                                        &mut self.agent, &self.client, self.cli, self.cfg, self.context,
                                                        &self.permission, &self.ask_tx, &self.sandbox, self.ui.reasoning_enabled,
                                                        &mut self.agent_run.agent_rx, &mut self.agent_run.main_abort, &mut self.agent_run.is_running,
                                                        &self.status_signals,
                                                        &mut self.wt_return_path,
                                                        #[cfg(feature = "mcp")] &mut self.mcp_manager,
                                                    ).await;
                                                }
                                                git_worktree::DeferredWorktreeAction::Exit { main_path } => {
                                                    std::env::set_current_dir(main_path)
                                                        .map_err(|e| anyhow::anyhow!("failed to change directory: {}", e))?;
                                                    self.session.working_dir = compact_str::CompactString::new(main_path);
                                                    self.context.reload();
                                                    apply_current_prompt_mode(self.context, &self.permission);
                                                    self.rebuild_agent().await;
                                                    render_session(&mut self.ui.renderer, self.session, self.cli, self.cfg, self.context)?;
                                                    self.ui.renderer.write_line(
                                                        &format!("returned to main repo at {}", main_path),
                                                        C_AGENT,
                                                    )?;
                                                }
                                            }
                                        }
                                        Err(e) if e.to_string().starts_with("DEFER_INIT:") => {
                                            let prompt = e.to_string().strip_prefix("DEFER_INIT:").unwrap_or("").to_string();
                                            #[cfg(feature = "mcp")]
                                            let mcp_ref = ensure_mcp_manager(&mut self.mcp_manager, self.cfg).await;
                                            ensure_agent(
                                                &mut self.agent, &self.client, self.session, self.cli, self.cfg, self.context,
                                                &self.permission, &self.ask_tx, &self.sandbox, self.ui.reasoning_enabled,
                                                #[cfg(feature = "mcp")] mcp_ref,
                                            ).await;
                                            let history = crate::agent::runner::convert_history(self.session);
                                            let runner = self.agent
                                                .as_ref()
                                                .unwrap()
                                                .clone()
                                                .spawn_runner(
                                                    prompt,
                                                    history,
                                                    self.cfg.retry.clone(),
                                                    #[cfg(feature = "hooks")]
                                                    None,
                                                )
                                                .await;
                                            self.agent_run.agent_rx = Some(runner.event_rx);
                                            self.agent_run.main_abort = Some(runner.abort_handle);
                                            self.agent_run.is_running = true;
                                            if let Some(ss) = self.status_signals.as_ref() {
                                                ss.send_start();
                                            }
                                        }
                                        Err(e) if e.to_string().starts_with("DEFER_REVIEW:") => {
                                            let msg = e.to_string().strip_prefix("DEFER_REVIEW:").unwrap_or("").to_string();
                                            self.chain.dot_prompt_restore = self.context.one_shot_restore.take();
                                            self.session.add_message(MessageRole::User, &msg);
                                            #[cfg(feature = "mcp")]
                                            let mcp_ref = ensure_mcp_manager(&mut self.mcp_manager, self.cfg).await;
                                            ensure_agent(
                                                &mut self.agent, &self.client, self.session, self.cli, self.cfg, self.context,
                                                &self.permission, &self.ask_tx, &self.sandbox, self.ui.reasoning_enabled,
                                                #[cfg(feature = "mcp")] mcp_ref,
                                            ).await;
                                            let history = crate::agent::runner::convert_history(self.session);
                                            let runner = self.agent
                                                .as_ref()
                                                .unwrap()
                                                .clone()
                                                .spawn_runner(
                                                    msg,
                                                    history,
                                                    self.cfg.retry.clone(),
                                                    #[cfg(feature = "hooks")]
                                                    None,
                                                )
                                                .await;
                                            self.agent_run.agent_rx = Some(runner.event_rx);
                                            self.agent_run.main_abort = Some(runner.abort_handle);
                                            self.agent_run.is_running = true;
                                            if let Some(ss) = self.status_signals.as_ref() {
                                                ss.send_start();
                                            }
                                        }
                                        Err(e) if e.to_string().starts_with("DEFER_EDITOR:") => {
                                            let path = e.to_string().strip_prefix("DEFER_EDITOR:").unwrap_or("").to_string();
                                            let editor = self.cfg.editor.clone()
                                                .or_else(|| std::env::var("EDITOR").ok())
                                                .unwrap_or_else(|| "editor".to_string());
                                            let _ = crossterm::terminal::disable_raw_mode();
                                            let mut stdout = std::io::stdout();
                                            let _ = crossterm::ExecutableCommand::execute(&mut stdout, crossterm::event::DisableMouseCapture);
                                            let _ = crossterm::ExecutableCommand::execute(&mut stdout, crossterm::terminal::LeaveAlternateScreen);
                                            let _ = std::io::Write::flush(&mut stdout);
                                            let _ = std::process::Command::new("sh")
                                                .arg("-c")
                                                .arg(format!("{} \"$1\"", editor))
                                                .arg("sh")
                                                .arg(&path)
                                                .status();
                                            let _ = crossterm::ExecutableCommand::execute(&mut stdout, crossterm::terminal::EnterAlternateScreen);
                                            let _ = crossterm::ExecutableCommand::execute(&mut stdout, crossterm::terminal::Clear(crossterm::terminal::ClearType::All));
                                            let _ = crossterm::ExecutableCommand::execute(&mut stdout, crossterm::event::EnableMouseCapture);
                                            let _ = crossterm::terminal::enable_raw_mode();
                                            render_session(&mut self.ui.renderer, self.session, self.cli, self.cfg, self.context)?;
                                            self.ui.renderer.write_line(&format!("returned from editing {}", path), C_AGENT)?;
                                        }
                                        Err(e) => {
                                            if e.downcast_ref::<std::io::Error>().is_some_and(|e: &std::io::Error| e.kind() == std::io::ErrorKind::Interrupted) {
                                                break;
                                            }
                                            self.ui.renderer.write_line(&format!("error: {}", e), C_ERROR)?;
                                        }
                                        Ok(_) => {
                                            if !self.cli.no_session
                                                && let Err(e) = crate::session::storage::save_session(self.session)
                                            {
                                                self.ui.renderer.write_line(
                                                    &format!("warning: failed to save session: {}", e),
                                                    C_ERROR,
                                                )?;
                                            }
                                            #[cfg(feature = "loop")]
                                            if let Some(ref mut ls) = self.loop_state
                                                && ls.active && ls.iteration == 0 && !self.agent_run.is_running
                                            {
                                                ls.iteration = 1;
                                                let prompt = ls.build_prompt();
                                                #[cfg(feature = "mcp")]
                                                let mcp_ref = ensure_mcp_manager(&mut self.mcp_manager, self.cfg).await;
                                                ensure_agent(
                                                    &mut self.agent, &self.client, self.session, self.cli, self.cfg, self.context,
                                                    &self.permission, &self.ask_tx, &self.sandbox, self.ui.reasoning_enabled,
                                                    #[cfg(feature = "mcp")] mcp_ref,
                                                ).await;
                                                let runner = self.agent
                                                    .as_ref()
                                                    .unwrap()
                                                    .clone()
                                                    .spawn_runner(
                                                        prompt,
                                                        Vec::new(),
                                                        self.cfg.retry.clone(),
                                                        #[cfg(feature = "hooks")]
                                                        Some(crate::extras::hooks::LoopInfo {
                                                            iteration: ls.iteration,
                                                            active: ls.active,
                                                        }),
                                                    )
                                                    .await;
                                                self.agent_run.agent_rx = Some(runner.event_rx);
                                                self.agent_run.main_abort = Some(runner.abort_handle);
                                                self.agent_run.is_running = true;
                                                self.loop_label = Some(ls.iteration_label());
                                            }
                                        }
                                    }
                                    if !self.cli.no_session
                                        && let Err(e) = crate::session::storage::save_session(self.session)
                                    {
                                        self.ui.renderer.write_line(
                                            &format!("warning: failed to save session: {}", e),
                                            C_ERROR,
                                        )?;
                                    }
                                } else if text.starts_with('!') {
                                    let cmd = text.strip_prefix('!').map(|s| s.trim()).unwrap_or("");
                                    if !cmd.is_empty() {
                                        for line in text.lines() {
                                            let safe_line = sanitize_output(line);
                                            self.ui.renderer.write_line(&format!("> {}", safe_line), crossterm::style::Color::Green)?;
                                        }
                                        self.ui.renderer.write_line("", crossterm::style::Color::White)?;

                                        let cmd_owned = cmd.to_string();
                                        let output = tokio::task::spawn_blocking(move || {
                                            std::process::Command::new("bash")
                                                .arg("-c")
                                                .arg(&cmd_owned)
                                                .output()
                                        })
                                        .await
                                        .map_err(|e| anyhow::anyhow!("spawn error: {}", e))?
                                        .map_err(|e| anyhow::anyhow!("command error: {}", e))?;

                                        let mut result = String::new();
                                        if !output.stdout.is_empty() {
                                            result.push_str(&String::from_utf8_lossy(&output.stdout));
                                        }
                                        if !output.stderr.is_empty() {
                                            if !result.is_empty() {
                                                result.push('\n');
                                            }
                                            result.push_str(&String::from_utf8_lossy(&output.stderr));
                                        }
                                        let result = result.trim().to_string();

                                        for line in result.lines() {
                                            let safe_line = sanitize_output(line);
                                            self.ui.renderer.write_line(
                                                &safe_line,
                                                if output.status.success() { C_AGENT } else { C_ERROR },
                                            )?;
                                        }
                                        self.ui.renderer.write_line("", crossterm::style::Color::White)?;

                                        self.session.add_message(MessageRole::User, &text);
                                        self.session.add_message(MessageRole::Assistant, &result);
                                        if !self.cli.no_session {
                                            let _ = crate::session::chat_history::append_entry(
                                                &crate::session::chat_history::ChatHistoryEntry {
                                                    content: text.to_string(),
                                                    timestamp: self.session.updated_at.clone(),
                                                },
                                            );
                                        }
                                    } else {
                                        self.ui.renderer.write_line("error: empty command after '!'", C_ERROR)?;
                                    }
                                } else {
                                    for line in text.lines() {
                                        let safe_line = sanitize_output(line);
                                        self.ui.renderer.write_line(&format!("> {}", safe_line), crossterm::style::Color::Green)?;
                                    }
                                    self.ui.renderer.write_line("", crossterm::style::Color::White)?;
                                    self.refresh()?;

                                    start_main_run(
                                        &text, &mut self.agent, &self.client, self.session, self.cli, self.cfg, self.context,
                                        &self.permission, &self.ask_tx, &self.sandbox, self.ui.reasoning_enabled,
                                        &mut self.agent_run.agent_rx, &mut self.agent_run.main_abort, &mut self.agent_run.is_running,
                                        &self.status_signals,
                                        #[cfg(feature = "mcp")] &mut self.mcp_manager,
                                        &mut self.prebuild_rx,
                                        &mut self.agent_run.pending_send,
                                    ).await;
                                }
                                }
                                self.refresh()?;
                            } else {
                                self.refresh()?;
                            }
                        }
                    }
                }
                Some(prebuilt) = async { self.prebuild_rx.as_mut()?.recv().await }, if self.agent.is_none() => {
                    #[cfg(feature = "mcp")]
                    {
                        let (built_agent, built_mcp) = prebuilt;
                        self.agent = Some(built_agent);
                        self.mcp_manager = built_mcp;
                        if let Some(m) = self.mcp_manager.as_mut() {
                            for notice in m.take_notices() {
                                self.ui.renderer.write_line(&notice, C_ERROR)?;
                            }
                        }
                    }
                    #[cfg(not(feature = "mcp"))]
                    {
                        self.agent = Some(prebuilt);
                    }
                    self.prebuild_rx = None;
                    self.refresh()?;
                    continue;
                }
                Some(event) = async {
                    self.agent_run.agent_rx.as_mut()?.recv().await
                } => {
                    match &event {
                        AgentEvent::ToolCall { name, args } => {
                            if self.agent_run.turn_trace.len() < TURN_TRACE_MAX {
                                self.agent_run.turn_trace.push(compact_str::CompactString::from(format!(
                                    "→ {}",
                                    super::utils::format_tool_call_summary(name, args)
                                )));
                            }
                        }
                        AgentEvent::ToolResult { output, .. } => {
                            if self.agent_run.turn_trace.len() < TURN_TRACE_MAX {
                                self.agent_run.turn_trace.push(compact_str::CompactString::from(format!(
                                    "← {}",
                                    crate::extras::truncate::truncate_cjk(output, 500, "…")
                                )));
                            }
                        }
                        AgentEvent::Done { .. } | AgentEvent::Error(_) => {
                            self.agent_run.turn_trace.clear();
                            self.agent_run.awaiting_compaction_relief = false;
                        }
                        _ => {}
                    }
                    #[cfg(feature = "loop")]
                    let loop_running = self.loop_state.as_ref().is_some_and(|ls| ls.active);
                    #[cfg(not(feature = "loop"))]
                    let loop_running = false;
                    if let AgentEvent::CompletionCall {
                        input_tokens,
                        cached_input_tokens,
                        cache_creation_input_tokens,
                        ..
                    } = &event
                        && self.agent_run.is_running
                        && !loop_running
                        && !self.cli.no_session
                        && self.cfg.resolve_compact_enabled()
                        && self.session.context_window > 0
                        && let Some(threshold) = self.cfg.resolve_mid_turn_compact_threshold()
                    {
                        let real_input_tokens = Session::real_input_tokens(
                            self.cfg.is_anthropic_native(&self.session.provider),
                            *input_tokens,
                            *cached_input_tokens,
                            *cache_creation_input_tokens,
                        );
                        let pressure = real_input_tokens as f64 / self.session.context_window as f64;
                        if pressure > threshold {
                            if self.agent_run.awaiting_compaction_relief {
                                stop_turn_context_exhausted(
                                    real_input_tokens, threshold, &mut self.ui.renderer, self.session, self.cfg,
                                    &mut self.agent_run.agent_rx, &mut self.agent_run.main_abort, &mut self.agent_run.is_running,
                                    &self.status_signals, &mut self.agent_run.turn_trace, &mut self.agent_run.response_buf,
                                    &mut self.agent_run.response_start_line, &mut self.agent_run.agent_line_started,
                                    &mut self.agent_run.was_reasoning,
                                )?;
                                self.agent_run.awaiting_compaction_relief = false;
                            } else {
                                mid_turn_compact_and_respawn(
                                    pressure, &mut self.ui.renderer, &mut self.agent, &mut self.client, self.session,
                                    self.cli, self.cfg, self.context, &self.permission, &self.ask_tx, &self.sandbox,
                                    self.ui.reasoning_enabled, &mut self.agent_run.agent_rx, &mut self.agent_run.main_abort,
                                    &mut self.agent_run.is_running, &self.status_signals, &mut self.agent_run.turn_trace,
                                    &mut self.agent_run.response_buf, &mut self.agent_run.response_start_line,
                                    &mut self.agent_run.agent_line_started, &mut self.agent_run.was_reasoning,
                                    #[cfg(feature = "mcp")] &mut self.mcp_manager,
                                ).await?;
                                self.agent_run.awaiting_compaction_relief = true;
                            }
                            self.refresh()?;
                            continue;
                        } else {
                            self.agent_run.awaiting_compaction_relief = false;
                        }
                    }
                    #[cfg(feature = "mcp")]
                    let mcp_ref = ensure_mcp_manager(&mut self.mcp_manager, self.cfg).await;
                    let turn_errored = matches!(&event, AgentEvent::Error(_));
                    handle_agent_event(
                        event, &mut self.ui.renderer, self.session, self.cfg, self.cli, self.context,
                        &mut self.agent_run.is_running, &mut self.agent_run.agent_rx, &mut self.agent_run.agent_line_started,
                        &mut self.agent_run.response_buf, &mut self.agent_run.response_start_line, &mut self.agent_run.was_reasoning,
                        self.ui.show_reasoning,
                        &mut self.agent, &mut self.client, &mut self.loop_label,
                        &self.permission, &self.ask_tx, &self.sandbox,
                        &self.status_signals,
                        #[cfg(feature = "loop")] &mut self.loop_state,
                        #[cfg(feature = "git-worktree")] &mut self.wt_return_path,
                        #[cfg(feature = "mcp")] mcp_ref,
                    ).await?;
                    if turn_errored {
                        if let Some(text) = self.agent_run.pending_send.take() {
                            let len = self.session.messages.len();
                            if len > 0 && self.session.messages[len - 1].role == MessageRole::User {
                                self.session.truncate_to(len - 1);
                            }
                            self.ui.input.buffer = text.into();
                            self.ui.input.cursor = self.ui.input.buffer.len();
                        }
                    } else if !self.agent_run.is_running {
                        self.agent_run.pending_send = None;
                    }
                    if !self.agent_run.is_running
                        && let Some(restore_name) = self.chain.dot_prompt_restore.take()
                    {
                        self.context.current_prompt = self.context.prompts.get(&restore_name).cloned();
                        self.context.current_prompt_name = if self.context.current_prompt.is_some() {
                            Some(restore_name)
                        } else {
                            None
                        };
                        if let Some(perm) = &self.permission {
                            let mut guard = perm.lock().unwrap_or_else(|e| e.into_inner());
                            guard.restore_user_mode();
                        }
                    }
                    if !self.agent_run.is_running
                        && self.chain.pending.is_none()
                        && let Some(ref name) = self.context.current_prompt_name
                        && !self.context.chain_declined.contains(name)
                        && let Some(phase) =
                            crate::extras::chain::ChainPhase::from_prompt_name(name)
                        && let Some(ref chain_cfg) = self.cfg.chain
                        && phase.is_enabled(chain_cfg)
                    {
                        self.chain.pending = Some(phase);
                        self.chain.label_msg =
                            Some(phase.chain_label().to_string());
                        self.ui.renderer.chain_but_mode = false;
                        self.ui.renderer.chain_prompt = Some(renderer::ChainPrompt {
                            question: compact_str::CompactString::from(phase.chain_label()),
                        });
                    }
                    if !self.agent_run.is_running {
                        self.agent_run.main_abort = None;
                        if let Some(next) = self.agent_run.pending_inputs.pop_front() {
                            self.ui.renderer.chain_prompt = None;
                            self.ui.renderer.chain_but_mode = false;
                            self.chain.pending = None;
                            self.chain.label_msg = None;
                            for line in next.lines() {
                                self.ui.renderer.write_line(&format!("> {}", sanitize_output(line)), crossterm::style::Color::Green)?;
                            }
                            self.ui.renderer.write_line("", crossterm::style::Color::White)?;
                            start_main_run(
                                &next, &mut self.agent, &self.client, self.session, self.cli, self.cfg, self.context,
                                &self.permission, &self.ask_tx, &self.sandbox, self.ui.reasoning_enabled,
                                &mut self.agent_run.agent_rx, &mut self.agent_run.main_abort, &mut self.agent_run.is_running,
                                &self.status_signals,
                                #[cfg(feature = "mcp")] &mut self.mcp_manager,
                                &mut self.prebuild_rx,
                                &mut self.agent_run.pending_send,
                            ).await;
                        }
                    }
                    self.refresh()?;
                }
                Some(ask_req) = async {
                    self.ask_rx.as_mut()?.recv().await
                } => {
                    handle_permission_request(
                        ask_req, &mut self.ui.renderer, self.session, self.cli,
                        &mut self.user_rx, &mut self.agent_run.agent_line_started, &mut self.agent_run.was_reasoning,
                    ).await?;
                    self.refresh()?;
                }
                Some(bev) = self.btw_rx.recv() => {
                    match bev {
                        BtwEvent::Done { id, response, input_tokens, output_tokens, cached_input_tokens, cache_creation_input_tokens } => {
                            self.btw.total_cost += crate::pricing::estimate_cost(
                                crate::pricing::billable_input_tokens(
                                    self.cfg.is_anthropic_native(&self.session.provider),
                                    input_tokens, cached_input_tokens, cache_creation_input_tokens,
                                ),
                                output_tokens,
                                self.session.input_token_cost, self.session.output_token_cost,
                            );
                            self.btw.total_in = self.btw.total_in.saturating_add(input_tokens);
                            self.btw.total_out = self.btw.total_out.saturating_add(output_tokens);
                            self.btw.abort.retain(|(i, _)| *i != id);
                            self.btw.inflight = self.btw.inflight.saturating_sub(1);
                            self.ui.renderer.write_line(&format!("[btw #{}] answer:", id), C_BTW)?;
                            for line in response.lines() {
                                self.ui.renderer.write_line(&sanitize_output(line), C_AGENT)?;
                            }
                            self.ui.renderer.write_line("", crossterm::style::Color::White)?;
                        }
                        BtwEvent::Error { id, message } => {
                            self.btw.abort.retain(|(i, _)| *i != id);
                            self.btw.inflight = self.btw.inflight.saturating_sub(1);
                            self.ui.renderer.write_line(&format!("[btw #{}] error: {}", id, sanitize_output(&message)), C_ERROR)?;
                        }
                    }
                    self.refresh()?;
                }
                _ = tokio::time::sleep(tokio::time::Duration::from_millis(100)), if self.agent_run.is_running => {
                    self.refresh()?;
                }
                else => {
                    if let Some(rx) = self.prebuild_rx.as_mut()
                        && self.agent.is_none()
                        && let Ok(payload) = rx.try_recv() {
                            #[cfg(feature = "mcp")]
                            {
                                self.agent = Some(payload.0);
                                self.mcp_manager = payload.1;
                            }
                            #[cfg(not(feature = "mcp"))]
                            {
                                self.agent = Some(payload);
                            }
                            self.prebuild_rx = None;
                        }
                    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
                }
            }

            #[cfg(feature = "advisor")]
            if let Some(ref mut rx) = self.handoff_rx
                && let Ok(handoff_req) = rx.try_recv()
            {
                super::handle_human_handoff(
                    handoff_req,
                    &mut self.ui.renderer,
                    &mut self.user_rx,
                    &mut self.agent_run.agent_line_started,
                    &mut self.agent_run.was_reasoning,
                )
                .await?;
                self.refresh()?;
            }
        }

        #[cfg(feature = "git-worktree")]
        if self.cli.resolve_wt_auto_merge(self.cfg)
            && let Some(info) = git_worktree::detect()
        {
            let target = git_worktree::default_branch(&info.main_repo_path)
                .unwrap_or_else(|| "main".to_string());

            let _ = self.ui.renderer.write_line(
                &format!(
                    "auto-merging worktree '{}' into '{}'...",
                    info.branch, target
                ),
                C_AGENT,
            );
            let mut proceed = true;
            if git_worktree::worktree_has_uncommitted(&info.worktree_path) {
                let _ = self.ui.renderer.write_line(
                    "worktree has uncommitted changes. [c]ommit all and continue  [a]bort merge",
                    C_PERM,
                );
                if let Some(ss) = self.status_signals.as_ref() {
                    ss.send_git_conflict();
                }
                let action = loop {
                    tokio::select! {
                        Some(ev) = self.user_rx.recv() => {
                            if let crate::event::UserEvent::Key(key) = ev {
                                match key.code {
                                    KeyCode::Char('c') | KeyCode::Char('C') => break 'c',
                                    KeyCode::Char('a') | KeyCode::Char('A') => break 'a',
                                    KeyCode::Enter | KeyCode::Esc => break 'a',
                                    _ => {}
                                }
                            }
                        }
                    }
                };
                match action {
                    'c' => {
                        if let Err(e) = git_worktree::worktree_auto_commit_all(&info.worktree_path)
                        {
                            let _ = self
                                .ui
                                .renderer
                                .write_line(&format!("auto-commit failed: {}", e), C_ERROR);
                            proceed = false;
                        } else {
                            let _ = self.ui.renderer.write_line(
                                "committed all worktree changes, proceeding with merge",
                                C_AGENT,
                            );
                        }
                    }
                    'a' => {
                        let _ = self
                            .ui
                            .renderer
                            .write_line("merge aborted, worktree left untouched", C_AGENT);
                        proceed = false;
                    }
                    _ => unreachable!(),
                }
            }
            let (state, outcome) = if proceed {
                git_worktree::try_merge(&info, &target)
            } else {
                (
                    git_worktree::MergeState {
                        info: info.clone(),
                        original_branch: String::new(),
                        orig_dir: std::path::PathBuf::new(),
                        stashed: false,
                    },
                    git_worktree::MergeOutcome::Error("aborted by user".into()),
                )
            };
            match outcome {
                git_worktree::MergeOutcome::Success => {
                    let merge_result = if self.cli.resolve_wt_force(self.cfg) {
                        git_worktree::complete_merge_force(&state)
                    } else {
                        git_worktree::complete_merge(&state)
                    };
                    match merge_result {
                        Ok(()) => {
                            let _ = self.ui.renderer.write_line(
                                &format!(
                                    "merged '{}' into '{}' and cleaned up",
                                    info.branch, target
                                ),
                                C_AGENT,
                            );
                        }
                        Err(e) => {
                            let _ = self.ui.renderer.write_line(
                                &format!("merge succeeded but cleanup failed: {}", e),
                                C_ERROR,
                            );
                        }
                    }
                }
                git_worktree::MergeOutcome::Conflicts(files) => {
                    let _ = self.ui.renderer.write_line(
                        &format!("merge conflict in {} file(s):", files.len()),
                        C_ERROR,
                    );
                    for f in &files {
                        let _ = self.ui.renderer.write_line(&format!("  {}", f), C_ERROR);
                    }
                    if let Some(ss) = self.status_signals.as_ref() {
                        ss.send_git_conflict();
                    }
                    let _ = self.ui.renderer.write_line(
                        "[a]bort  [l]eave for manual resolution  [h]elp (agent resolves)",
                        C_PERM,
                    );

                    let action = loop {
                        tokio::select! {
                            Some(ev) = self.user_rx.recv() => {
                                if let crate::event::UserEvent::Key(key) = ev {
                                    match key.code {
                                        KeyCode::Char('a') | KeyCode::Char('A') => break 'a',
                                        KeyCode::Char('l') | KeyCode::Char('L') => break 'l',
                                        KeyCode::Char('h') | KeyCode::Char('H') => break 'h',
                                        KeyCode::Enter | KeyCode::Esc => break 'a',
                                        _ => {}
                                    }
                                }
                            }
                        }
                    };

                    match action {
                        'a' => {
                            let _ = git_worktree::cancel_merge(&state);
                            git_worktree::cleanup_worktree(
                                &info.worktree_path.to_string_lossy(),
                                &info.branch,
                                &info.main_repo_path.to_string_lossy(),
                                self.cli.resolve_wt_force(self.cfg),
                            );
                            let _ = self
                                .ui
                                .renderer
                                .write_line("merge aborted, restored original state", C_AGENT);
                        }
                        'l' => {
                            let _ = self.ui.renderer.write_line(
                                &format!(
                                    "conflict state left in {} for manual resolution",
                                    info.main_repo_path.display()
                                ),
                                C_AGENT,
                            );
                        }
                        'h' => {
                            let _ = git_worktree::cancel_merge(&state);
                            let _ = self
                                .ui
                                .renderer
                                .write_line("agent resolving merge...", C_AGENT);
                            let main_path = info.main_repo_path.display().to_string();
                            let wt_path = info.worktree_path.display().to_string();
                            let force_flag = self.cli.resolve_wt_force(self.cfg);
                            spawn_merge_agent(
                                &info.branch,
                                &target,
                                &main_path,
                                &wt_path,
                                force_flag,
                                self.session,
                                &mut self.agent,
                                &self.client,
                                self.cli,
                                self.cfg,
                                self.context,
                                &self.permission,
                                &self.ask_tx,
                                &self.sandbox,
                                self.ui.reasoning_enabled,
                                &mut self.agent_run.agent_rx,
                                &mut self.agent_run.main_abort,
                                &mut self.agent_run.is_running,
                                &self.status_signals,
                                &mut self.wt_return_path,
                                #[cfg(feature = "mcp")]
                                &mut self.mcp_manager,
                            )
                            .await;

                            let mut merge_agent_line_started = false;
                            let mut merge_response_buf = String::new();
                            let mut merge_response_start_line = None;
                            let mut merge_was_reasoning = false;
                            while self.agent_run.is_running {
                                let ev = match self.agent_run.agent_rx.as_mut() {
                                    Some(rx) => {
                                        tokio::select! {
                                            Some(e) = rx.recv() => Some(e),
                                            Some(ev) = self.user_rx.recv() => {
                                                if let crate::event::UserEvent::Key(key) = ev {
                                                    let is_ctrl_c = key.code == KeyCode::Char('c')
                                                        && key.modifiers.contains(KeyModifiers::CONTROL);
                                                    if is_ctrl_c {
                                                        if let Some(h) = self.agent_run.main_abort.take() {
                                                            h.abort();
                                                        }
                                                        self.sandbox.kill_active();
                                                        self.agent_run.is_running = false;
                                                        if let Some(ss) = self.status_signals.as_ref() {
                                                            ss.send_stop();
                                                        }
                                                        self.agent_run.agent_rx = None;
                                                    }
                                                }
                                                None
                                            }
                                            Some(ask_req) = async {
                                                if let Some(rx) = self.ask_rx.as_mut() {
                                                    rx.recv().await
                                                } else {
                                                    std::future::pending().await
                                                }
                                            } => {
                                                let _ = handle_permission_request(
                                                    ask_req, &mut self.ui.renderer, self.session, self.cli,
                                                    &mut self.user_rx, &mut merge_agent_line_started,
                                                    &mut merge_was_reasoning,
                                                ).await;
                                                None
                                            }
                                        }
                                    }
                                    None => break,
                                };
                                if let Some(ev) = ev {
                                    #[cfg(feature = "mcp")]
                                    let mcp_ref =
                                        ensure_mcp_manager(&mut self.mcp_manager, self.cfg).await;
                                    handle_agent_event(
                                        ev,
                                        &mut self.ui.renderer,
                                        self.session,
                                        self.cfg,
                                        self.cli,
                                        self.context,
                                        &mut self.agent_run.is_running,
                                        &mut self.agent_run.agent_rx,
                                        &mut merge_agent_line_started,
                                        &mut merge_response_buf,
                                        &mut merge_response_start_line,
                                        &mut merge_was_reasoning,
                                        self.ui.show_reasoning,
                                        &mut self.agent,
                                        &mut self.client,
                                        &mut self.loop_label,
                                        &self.permission,
                                        &self.ask_tx,
                                        &self.sandbox,
                                        &self.status_signals,
                                        #[cfg(feature = "loop")]
                                        &mut self.loop_state,
                                        #[cfg(feature = "git-worktree")]
                                        &mut self.wt_return_path,
                                        #[cfg(feature = "mcp")]
                                        mcp_ref,
                                    )
                                    .await?;
                                }
                            }
                        }
                        _ => unreachable!(),
                    }
                }
                git_worktree::MergeOutcome::Error(e) => {
                    let _ = self
                        .ui
                        .renderer
                        .write_line(&format!("merge failed: {}", e), C_ERROR);
                }
            }
        }

        self.running.store(false, Ordering::Relaxed);
        if let Some(h) = self.event_handle.take() {
            let _ = h.join();
        }

        #[cfg(feature = "mcp")]
        if let Some(mgr) = self.mcp_manager.take() {
            mgr.shutdown().await;
        }

        Ok(())
    }
}
