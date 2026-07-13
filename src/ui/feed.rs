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
}
