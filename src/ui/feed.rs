use compact_str::CompactString;
use crossterm::style::Color;

use crate::session::MessageRole;
use crate::session::Session;

use super::markdown;
use super::renderer::LineEntry;

/// A structured conversation block in the feed.
#[derive(Clone, Debug)]
pub enum FeedBlock {
    User {
        text: CompactString,
    },
    Assistant {
        text: CompactString,
    },
    ToolCall {
        name: CompactString,
        args: CompactString,
    },
    ToolResult {
        raw: CompactString,
        label: CompactString,
        body: CompactString,
    },
    SubagentToolCall {
        text: CompactString,
    },
    System {
        text: CompactString,
    },
    Error {
        text: CompactString,
    },
    Line {
        text: CompactString,
        color: Color,
    },
    Blank,
}

/// Structured feed of conversation blocks, separate from the renderer.
#[derive(Clone, Debug, Default)]
pub struct Feed {
    blocks: Vec<FeedBlock>,
    total_visual_lines: usize,
}

impl Feed {
    pub fn new() -> Self {
        Self {
            blocks: Vec::new(),
            total_visual_lines: 0,
        }
    }

    pub fn push_block(&mut self, block: FeedBlock) {
        self.blocks.push(block);
    }

    pub fn push_line(&mut self, text: &str, color: Color) {
        self.blocks.push(FeedBlock::Line {
            text: CompactString::new(text),
            color,
        });
    }

    pub fn push_blank(&mut self) {
        self.blocks.push(FeedBlock::Blank);
    }

    pub fn clear(&mut self) {
        self.blocks.clear();
        self.total_visual_lines = 0;
    }

    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    pub fn blocks(&self) -> &[FeedBlock] {
        &self.blocks
    }

    pub fn lines(&mut self, width: usize) -> Vec<LineEntry> {
        let mut entries = Vec::new();
        for block in &self.blocks {
            match block {
                FeedBlock::User { text } => {
                    for line in text.lines() {
                        entries.push(LineEntry {
                            text: CompactString::from(format!("> {}", line)),
                            color: Color::Green,
                        });
                    }
                }
                FeedBlock::Assistant { text } => {
                    let max_width = width.saturating_sub(1);
                    let mut styled = markdown::markdown_to_styled(text, max_width);
                    if !styled.is_empty() {
                        styled[0].text = CompactString::from(format!("< {}", styled[0].text));
                    }
                    for entry in styled {
                        entries.push(LineEntry {
                            text: entry.text,
                            color: entry.color,
                        });
                    }
                }
                FeedBlock::ToolCall { name, args } => {
                    entries.push(LineEntry {
                        text: CompactString::from(format!("◈ {} {}", name, args)),
                        color: Color::Yellow,
                    });
                }
                FeedBlock::ToolResult { label, body, .. } => {
                    entries.push(LineEntry {
                        text: label.clone(),
                        color: Color::DarkGrey,
                    });
                    for line in body.lines() {
                        entries.push(LineEntry {
                            text: CompactString::new(line),
                            color: Color::DarkGrey,
                        });
                    }
                }
                FeedBlock::SubagentToolCall { text } => {
                    entries.push(LineEntry {
                        text: CompactString::from(format!("⌥ {}", text)),
                        color: Color::Yellow,
                    });
                }
                FeedBlock::System { text } => {
                    for line in text.lines() {
                        entries.push(LineEntry {
                            text: CompactString::from(format!("# {}", line)),
                            color: Color::DarkGrey,
                        });
                    }
                }
                FeedBlock::Error { text } => {
                    for line in text.lines() {
                        entries.push(LineEntry {
                            text: CompactString::new(line),
                            color: Color::Red,
                        });
                    }
                }
                FeedBlock::Line { text, color } => {
                    entries.push(LineEntry {
                        text: text.clone(),
                        color: *color,
                    });
                }
                FeedBlock::Blank => {
                    entries.push(LineEntry {
                        text: CompactString::default(),
                        color: Color::White,
                    });
                }
            }
        }
        self.total_visual_lines = entries.len();
        entries
    }

    pub fn total_visual_lines(&self) -> usize {
        self.total_visual_lines
    }

    pub fn from_session(session: &Session, width: usize) -> Self {
        let mut feed = Self::new();
        for msg in &session.messages {
            match msg.role {
                MessageRole::User => {
                    feed.push_block(FeedBlock::User {
                        text: CompactString::new(&msg.content),
                    });
                }
                MessageRole::Assistant => {
                    feed.push_block(FeedBlock::Assistant {
                        text: CompactString::new(&msg.content),
                    });
                }
                MessageRole::System => {
                    feed.push_block(FeedBlock::System {
                        text: CompactString::new(&msg.content),
                    });
                }
                MessageRole::ToolCall => {
                    feed.push_block(FeedBlock::ToolCall {
                        name: CompactString::default(),
                        args: CompactString::new(&msg.content),
                    });
                }
                MessageRole::ToolResult => {
                    feed.push_block(FeedBlock::ToolResult {
                        raw: CompactString::new(&msg.content),
                        label: CompactString::new(""),
                        body: CompactString::new(&msg.content),
                    });
                }
                MessageRole::SubagentToolCall => {
                    feed.push_block(FeedBlock::SubagentToolCall {
                        text: CompactString::new(&msg.content),
                    });
                }
            }
            feed.push_blank();
        }
        feed.lines(width);
        feed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_feed_empty() {
        let mut feed = Feed::new();
        let lines = feed.lines(80);
        assert!(lines.is_empty());
    }

    #[test]
    fn test_feed_user_block() {
        let mut feed = Feed::new();
        feed.push_block(FeedBlock::User {
            text: CompactString::new("hello"),
        });
        let lines = feed.lines(80);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "> hello");
        assert_eq!(lines[0].color, Color::Green);
    }

    #[test]
    fn test_feed_user_multiline() {
        let mut feed = Feed::new();
        feed.push_block(FeedBlock::User {
            text: CompactString::new("line1\nline2"),
        });
        let lines = feed.lines(80);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].text, "> line1");
        assert_eq!(lines[1].text, "> line2");
        assert_eq!(lines[0].color, Color::Green);
    }

    #[test]
    fn test_feed_assistant_block() {
        let mut feed = Feed::new();
        feed.push_block(FeedBlock::Assistant {
            text: CompactString::new("hi"),
        });
        let lines = feed.lines(80);
        assert!(!lines.is_empty());
        assert!(lines[0].text.contains("hi"));
        assert!(lines[0].text.starts_with("<"));
    }

    #[test]
    fn test_feed_tool_call_block() {
        let mut feed = Feed::new();
        feed.push_block(FeedBlock::ToolCall {
            name: CompactString::new("bash"),
            args: CompactString::new("echo hi"),
        });
        let lines = feed.lines(80);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "◈ bash echo hi");
        assert_eq!(lines[0].color, Color::Yellow);
    }

    #[test]
    fn test_feed_tool_result_block() {
        let mut feed = Feed::new();
        feed.push_block(FeedBlock::ToolResult {
            raw: CompactString::new("ignore"),
            label: CompactString::new("ok"),
            body: CompactString::new("line1\nline2"),
        });
        let lines = feed.lines(80);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].text, "ok");
        assert_eq!(lines[0].color, Color::DarkGrey);
        assert_eq!(lines[1].text, "line1");
        assert_eq!(lines[2].text, "line2");
    }

    #[test]
    fn test_feed_subagent_tool_call() {
        let mut feed = Feed::new();
        feed.push_block(FeedBlock::SubagentToolCall {
            text: CompactString::new("find all todos"),
        });
        let lines = feed.lines(80);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "⌥ find all todos");
        assert_eq!(lines[0].color, Color::Yellow);
    }

    #[test]
    fn test_feed_system_block() {
        let mut feed = Feed::new();
        feed.push_block(FeedBlock::System {
            text: CompactString::new("note"),
        });
        let lines = feed.lines(80);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "# note");
        assert_eq!(lines[0].color, Color::DarkGrey);
    }

    #[test]
    fn test_feed_system_multiline() {
        let mut feed = Feed::new();
        feed.push_block(FeedBlock::System {
            text: CompactString::new("a\nb"),
        });
        let lines = feed.lines(80);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].text, "# a");
        assert_eq!(lines[1].text, "# b");
    }

    #[test]
    fn test_feed_error_block() {
        let mut feed = Feed::new();
        feed.push_block(FeedBlock::Error {
            text: CompactString::new("something went wrong"),
        });
        let lines = feed.lines(80);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "something went wrong");
        assert_eq!(lines[0].color, Color::Red);
    }

    #[test]
    fn test_feed_error_multiline() {
        let mut feed = Feed::new();
        feed.push_block(FeedBlock::Error {
            text: CompactString::new("err1\nerr2"),
        });
        let lines = feed.lines(80);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].text, "err1");
        assert_eq!(lines[1].text, "err2");
        assert_eq!(lines[0].color, Color::Red);
    }

    #[test]
    fn test_feed_line_block() {
        let mut feed = Feed::new();
        feed.push_block(FeedBlock::Line {
            text: CompactString::new("custom"),
            color: Color::Blue,
        });
        let lines = feed.lines(80);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "custom");
        assert_eq!(lines[0].color, Color::Blue);
    }

    #[test]
    fn test_feed_blank() {
        let mut feed = Feed::new();
        feed.push_block(FeedBlock::Blank);
        let lines = feed.lines(80);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "");
        assert_eq!(lines[0].color, Color::White);
    }

    #[test]
    fn test_feed_push_line() {
        let mut feed = Feed::new();
        feed.push_line("hello", Color::Magenta);
        let lines = feed.lines(80);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "hello");
        assert_eq!(lines[0].color, Color::Magenta);
    }

    #[test]
    fn test_feed_push_blank() {
        let mut feed = Feed::new();
        feed.push_blank();
        assert_eq!(feed.blocks().len(), 1);
    }

    #[test]
    fn test_feed_clear() {
        let mut feed = Feed::new();
        feed.push_line("x", Color::White);
        let _ = feed.lines(80);
        assert!(!feed.is_empty());
        feed.clear();
        assert!(feed.is_empty());
        assert_eq!(feed.total_visual_lines(), 0);
    }

    #[test]
    fn test_feed_is_empty() {
        let mut feed = Feed::new();
        assert!(feed.is_empty());
        feed.push_blank();
        assert!(!feed.is_empty());
    }

    #[test]
    fn test_feed_total_visual_lines() {
        let mut feed = Feed::new();
        feed.push_block(FeedBlock::User {
            text: CompactString::new("a\nb\nc"),
        });
        let lines = feed.lines(80);
        assert_eq!(feed.total_visual_lines(), lines.len());
        assert_eq!(feed.total_visual_lines(), 3);
    }

    #[test]
    fn test_feed_multiple_blocks() {
        let mut feed = Feed::new();
        feed.push_block(FeedBlock::User {
            text: CompactString::new("hello"),
        });
        feed.push_block(FeedBlock::Blank);
        feed.push_block(FeedBlock::Assistant {
            text: CompactString::new("world"),
        });
        let lines = feed.lines(80);
        assert!(lines.len() >= 3);
        assert_eq!(lines[0].text, "> hello");
        assert_eq!(lines[1].text, "");
    }

    #[test]
    fn test_feed_from_session() {
        use crate::session::SessionMessage;
        let mut session = Session::new("test", "test", 8192, "test");
        session.messages.push(SessionMessage {
            role: MessageRole::User,
            content: CompactString::new("hi"),
            estimated_tokens: 1,
        });
        session.messages.push(SessionMessage {
            role: MessageRole::Assistant,
            content: CompactString::new("hello"),
            estimated_tokens: 1,
        });
        let feed = Feed::from_session(&session, 80);
        assert!(!feed.is_empty());
        let blocks = feed.blocks();
        assert_eq!(blocks.len(), 4);
        assert!(matches!(blocks[0], FeedBlock::User { .. }));
        assert!(matches!(blocks[1], FeedBlock::Blank));
        assert!(matches!(blocks[2], FeedBlock::Assistant { .. }));
        assert!(matches!(blocks[3], FeedBlock::Blank));
    }

    #[test]
    fn test_feed_from_session_all_roles() {
        use crate::session::SessionMessage;
        let mut session = Session::new("test", "test", 8192, "test");
        let roles = [
            (MessageRole::User, "u"),
            (MessageRole::Assistant, "a"),
            (MessageRole::System, "s"),
            (MessageRole::ToolCall, "tc"),
            (MessageRole::ToolResult, "tr"),
            (MessageRole::SubagentToolCall, "sac"),
        ];
        for (role, content) in &roles {
            session.messages.push(SessionMessage {
                role: role.clone(),
                content: CompactString::new(*content),
                estimated_tokens: 1,
            });
        }
        let feed = Feed::from_session(&session, 80);
        let blocks = feed.blocks();
        assert_eq!(blocks.len(), 12);
        assert!(matches!(blocks[0], FeedBlock::User { .. }));
        assert!(matches!(blocks[2], FeedBlock::Assistant { .. }));
        assert!(matches!(blocks[4], FeedBlock::System { .. }));
        assert!(matches!(blocks[6], FeedBlock::ToolCall { .. }));
        assert!(matches!(blocks[8], FeedBlock::ToolResult { .. }));
        assert!(matches!(blocks[10], FeedBlock::SubagentToolCall { .. }));
    }
}
