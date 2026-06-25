//! Configurable status-bar statusline.
//!
//! The statusline is up to 3 lines, each an ordered list of segments parsed from
//! `[statusline]` in config (see `docs/CONFIG.md`). When no `[statusline]` is set, a
//! built-in default layout is used. Items resolve to text + colors at render
//! time; `separator` is literal text and `flex_separator` expands to fill the
//! row, pushing later segments to the right.

use std::sync::OnceLock;

use crossterm::style::Color;

use crate::config::Config;
use crate::config::types::{IconSpec, StatusLineConfig, StatusLineLine, StatusLineSegment};
use crate::session::{GitStatus, Session};
use crate::ui::utils::parse_color;

pub const MAX_STATUS_LINES: usize = 3;

/// A drawable statusline piece after items are resolved.
#[derive(Clone, Debug)]
pub enum StatusSpan {
    Text {
        text: String,
        fg: Option<Color>,
        bg: Option<Color>,
    },
    /// Expands to fill remaining width (splits evenly when several are present).
    Flex,
}

/// Runtime values the statusline can show beyond the session itself.
pub struct StatusContext<'a> {
    pub loop_label: Option<&'a str>,
    pub prompt_name: Option<&'a str>,
    pub perm_mode: Option<&'a str>,
    pub chain_label: Option<&'a str>,
    pub btw_cost: f64,
    pub btw_in: u64,
    pub btw_out: u64,
}

static SPEC: OnceLock<StatusLineConfig> = OnceLock::new();
static NEEDS_GIT_STATUS: OnceLock<bool> = OnceLock::new();

/// Parse the statusline spec from config once at startup. Clamps to 3 lines.
pub fn init(cfg: &Config) {
    let mut spec = cfg.statusline.clone().unwrap_or_else(default_spec);
    if spec.lines.is_empty() {
        spec = default_spec();
    }
    spec.lines.truncate(MAX_STATUS_LINES);
    let needs_git = spec.lines.iter().any(|l| {
        l.segments
            .iter()
            .any(|s| matches!(s.item.as_str(), "git_changes" | "git_status"))
    });
    let _ = SPEC.set(spec);
    let _ = NEEDS_GIT_STATUS.set(needs_git);
}

fn spec() -> &'static StatusLineConfig {
    SPEC.get_or_init(default_spec)
}

/// Number of statusline lines (1-3).
pub fn line_count() -> usize {
    spec().lines.len().clamp(1, MAX_STATUS_LINES)
}

/// Whether the configured statusline needs `git status` (a subprocess). False lets
/// the caller skip computing it.
pub fn needs_git_status() -> bool {
    *NEEDS_GIT_STATUS.get_or_init(|| false)
}

/// Build the statusline's drawable lines for the current state.
pub fn build(session: &Session, ctx: &StatusContext) -> Vec<Vec<StatusSpan>> {
    build_lines(spec(), session, ctx)
}

/// Build drawable lines from an explicit spec (used by `build` and tests).
pub fn build_lines(
    spec: &StatusLineConfig,
    session: &Session,
    ctx: &StatusContext,
) -> Vec<Vec<StatusSpan>> {
    spec.lines
        .iter()
        .map(|line| build_line(line, session, ctx))
        .collect()
}

fn color(c: &Option<compact_str::CompactString>) -> Option<Color> {
    c.as_ref().and_then(|s| parse_color(s))
}

fn build_line(line: &StatusLineLine, session: &Session, ctx: &StatusContext) -> Vec<StatusSpan> {
    // (span, is_separator) so we can trim separators around skipped items.
    let mut raw: Vec<(StatusSpan, bool)> = Vec::new();
    for seg in &line.segments {
        match seg.item.as_str() {
            "flex_separator" | "flex" => raw.push((StatusSpan::Flex, false)),
            "separator" | "sep" => {
                let text = seg
                    .text
                    .as_ref()
                    .map(|t| t.to_string())
                    .unwrap_or_else(|| " ".to_string());
                raw.push((
                    StatusSpan::Text {
                        text,
                        fg: color(&seg.color),
                        bg: color(&seg.bg),
                    },
                    true,
                ));
            }
            item => {
                if let Some(mut text) = resolve_item(item, session, ctx) {
                    if let Some(glyph) = resolve_icon(seg.icon.as_ref(), item) {
                        text = format!("{glyph} {text}");
                    }
                    let fg = color(&seg.color);
                    let bg = color(&seg.bg);
                    // Powerline caps: the glyph is drawn in the segment's bg color
                    // (so it reads as the segment's rounded/triangle edge) over the
                    // status-bar background. Falls back to the fg when no bg is set.
                    let cap = bg.or(fg);
                    if let Some(l) = &seg.left {
                        raw.push((
                            StatusSpan::Text {
                                text: powerline_glyph(l),
                                fg: cap,
                                bg: None,
                            },
                            false,
                        ));
                    }
                    raw.push((StatusSpan::Text { text, fg, bg }, false));
                    if let Some(r) = &seg.right {
                        raw.push((
                            StatusSpan::Text {
                                text: powerline_glyph(r),
                                fg: cap,
                                bg: None,
                            },
                            false,
                        ));
                    }
                }
            }
        }
    }

    // Drop leading/duplicate separators (a separator whose previous kept piece
    // is also a separator), then any trailing separators. This keeps layouts
    // clean when optional items (cost, mode, git) resolve to nothing.
    let mut cleaned: Vec<(StatusSpan, bool)> = Vec::with_capacity(raw.len());
    for (span, is_sep) in raw {
        if is_sep {
            let prev_is_sep = cleaned.last().is_none_or(|(_, s)| *s);
            if prev_is_sep {
                continue;
            }
        }
        cleaned.push((span, is_sep));
    }
    while matches!(cleaned.last(), Some((_, true))) {
        cleaned.pop();
    }
    cleaned.into_iter().map(|(s, _)| s).collect()
}

/// Resolve a non-separator item to display text, or `None` to skip it.
fn resolve_item(item: &str, session: &Session, ctx: &StatusContext) -> Option<String> {
    match item {
        "session_name" => {
            let n = session.name.as_str();
            (!n.is_empty()).then(|| n.to_string())
        }
        "session_id" => Some(session.id.chars().take(8).collect()),
        "git_branch" => session.git_branch.as_ref().map(|b| b.to_string()),
        "git_changes" => session.git_status.as_ref().and_then(format_changes),
        "git_status" => session.git_status.as_ref().map(format_status),
        "cwd" => Some(basename(&session.working_dir)),
        "model" => Some(session.model.to_string()),
        "tokens_input" => {
            (session.total_input_tokens > 0).then(|| fmt_tokens(session.total_input_tokens))
        }
        "tokens_output" => {
            (session.total_output_tokens > 0).then(|| fmt_tokens(session.total_output_tokens))
        }
        "context_used" => {
            // A `~` marks the figure as an estimate until the provider reports
            // real usage (it then snaps to the exact number).
            let mark = if session.ctx_is_estimated() { "~" } else { "" };
            Some(format!(
                "{mark}{}",
                fmt_tokens(session.effective_context_tokens())
            ))
        }
        "context_max" => Some(fmt_tokens(session.context_window)),
        "context_percentage" => {
            let pct = (session.effective_context_tokens() * 100)
                .checked_div(session.context_window)
                .unwrap_or(0);
            Some(format!("{pct}%"))
        }
        "cost" => (session.total_cost > 0.0 || session.show_cost_always)
            .then(|| format!("${:.4}", session.total_cost)),
        "prompt" => ctx.prompt_name.map(|s| format!("prompt:{s}")),
        "mode" => ctx
            .perm_mode
            .filter(|m| *m != "standard")
            .map(|m| format!("mode:{m}")),
        "loop" => ctx.loop_label.map(|s| format!("[{s}]")),
        "chain" => ctx.chain_label.map(|s| s.to_string()),
        "compaction" => {
            (!session.compactions.is_empty()).then(|| format!("cmp:{}", session.compactions.len()))
        }
        "btw" => {
            if ctx.btw_in == 0 && ctx.btw_out == 0 {
                None
            } else if ctx.btw_cost > 0.0 {
                Some(format!(
                    "btw:${:.4} ({}/{})",
                    ctx.btw_cost,
                    fmt_tokens(ctx.btw_in),
                    fmt_tokens(ctx.btw_out)
                ))
            } else {
                Some(format!(
                    "btw:{}/{}",
                    fmt_tokens(ctx.btw_in),
                    fmt_tokens(ctx.btw_out)
                ))
            }
        }
        _ => None,
    }
}

fn basename(dir: &str) -> String {
    std::path::Path::new(dir)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(dir)
        .to_string()
}

/// `+staged ~modified -deleted ?untracked`, only the non-zero parts; `None`
/// when the tree is clean.
pub fn format_changes(g: &GitStatus) -> Option<String> {
    if !g.is_dirty() {
        return None;
    }
    let mut parts: Vec<String> = Vec::new();
    if g.staged > 0 {
        parts.push(format!("+{}", g.staged));
    }
    if g.modified > 0 {
        parts.push(format!("~{}", g.modified));
    }
    if g.deleted > 0 {
        parts.push(format!("-{}", g.deleted));
    }
    if g.untracked > 0 {
        parts.push(format!("?{}", g.untracked));
    }
    Some(parts.join(" "))
}

/// Upstream sync plus a clean/dirty marker: `↑1 ↓2 *`, or `✓` when clean and
/// in sync.
pub fn format_status(g: &GitStatus) -> String {
    let mut parts: Vec<String> = Vec::new();
    if g.ahead > 0 {
        parts.push(format!("\u{2191}{}", g.ahead));
    }
    if g.behind > 0 {
        parts.push(format!("\u{2193}{}", g.behind));
    }
    if g.is_dirty() {
        parts.push("*".to_string());
    }
    if parts.is_empty() {
        "\u{2713}".to_string()
    } else {
        parts.join(" ")
    }
}

/// Resolve a segment's `icon` setting to a glyph: `Auto(true)` uses the item's
/// built-in icon, a custom string is looked up by name (or used literally), and
/// anything else yields no icon. Needs a Nerd Font to render.
fn resolve_icon(icon: Option<&IconSpec>, item: &str) -> Option<String> {
    match icon {
        Some(IconSpec::Auto(true)) => item_icon(item).map(|g| g.to_string()),
        Some(IconSpec::Custom(s)) => Some(icon_glyph(s)),
        _ => None,
    }
}

/// Built-in Nerd Font icon for an item, when one fits.
pub fn item_icon(item: &str) -> Option<&'static str> {
    let g = match item {
        "git_branch" => "\u{e0a0}",                                          //
        "git_changes" => "\u{f044}",                                         //
        "git_status" => "\u{f021}",                                          //
        "cwd" => "\u{f07b}",                                                 //
        "model" => "\u{f2db}",                                               //
        "cost" => "\u{f155}",                                                //
        "context_used" | "context_max" | "context_percentage" => "\u{f1c0}", //
        "session_name" | "session_id" => "\u{f292}",                         //
        "prompt" => "\u{f120}",                                              //
        "mode" => "\u{f023}",                                                //
        "loop" => "\u{f01e}",                                                //
        "btw" => "\u{f075}",                                                 //
        "compaction" => "\u{f066}",                                          //
        _ => return None,
    };
    Some(g)
}

/// Named icon lookup, with passthrough so any literal glyph also works.
pub fn icon_glyph(name: &str) -> String {
    let g = match name {
        "branch" | "git" => "\u{e0a0}",
        "folder" | "dir" => "\u{f07b}",
        "chip" | "model" => "\u{f2db}",
        "dollar" | "money" => "\u{f155}",
        "database" | "context" => "\u{f1c0}",
        "hash" => "\u{f292}",
        "terminal" => "\u{f120}",
        "lock" => "\u{f023}",
        "pencil" | "edit" => "\u{f044}",
        "sync" | "refresh" => "\u{f021}",
        other => other,
    };
    g.to_string()
}

/// Resolve a powerline cap name to its glyph, or return the input unchanged so
/// any literal string (including a raw Nerd Font codepoint) also works. These
/// glyphs need a Nerd Font / Powerline-patched font to render.
pub fn powerline_glyph(name: &str) -> String {
    let g = match name {
        "pl_right" | "powerline_right" => "\u{e0b0}",   //
        "pl_left" | "powerline_left" => "\u{e0b2}",     //
        "pl_right_thin" => "\u{e0b1}",                  //
        "pl_left_thin" => "\u{e0b3}",                   //
        "pl_round_right" | "round_right" => "\u{e0b4}", //
        "pl_round_left" | "round_left" => "\u{e0b6}",   //
        "pl_flame_right" => "\u{e0c0}",
        "pl_flame_left" => "\u{e0c2}",
        other => other,
    };
    g.to_string()
}

pub fn fmt_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{}k", n / 1000)
    } else {
        n.to_string()
    }
}

/// Built-in single-line layout used when `[statusline]` is not configured.
pub fn default_spec() -> StatusLineConfig {
    fn seg(item: &str, c: Option<&str>) -> StatusLineSegment {
        StatusLineSegment {
            item: item.into(),
            color: c.map(|s| s.into()),
            ..Default::default()
        }
    }
    fn sep(text: &str) -> StatusLineSegment {
        StatusLineSegment {
            item: "separator".into(),
            text: Some(text.into()),
            ..Default::default()
        }
    }
    let segments = vec![
        seg("cwd", Some("light_blue")),
        sep(" "),
        seg("git_branch", Some("magenta")),
        sep(" "),
        seg("git_changes", Some("yellow")),
        sep("  |  "),
        seg("model", Some("white")),
        sep("  |  "),
        seg("context_used", Some("green")),
        sep("/"),
        seg("context_max", Some("green")),
        sep(" "),
        seg("context_percentage", Some("green")),
        sep("  "),
        seg("tokens_input", Some("cyan")),
        sep("/"),
        seg("tokens_output", Some("cyan")),
        StatusLineSegment {
            item: "flex_separator".into(),
            ..Default::default()
        },
        seg("loop", Some("dark_yellow")),
        sep(" "),
        seg("mode", Some("red")),
        sep(" "),
        seg("cost", Some("green")),
        sep(" "),
        seg("btw", Some("dark_cyan")),
        sep(" "),
        seg("prompt", Some("dark_grey")),
    ];
    StatusLineConfig {
        lines: vec![StatusLineLine { segments }],
    }
}
