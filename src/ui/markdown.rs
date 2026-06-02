use compact_str::CompactString;
use crossterm::style::Color;
use pulldown_cmark::{Alignment, Event, Options, Tag, TagEnd};
use smallvec::{SmallVec, smallvec};

use super::renderer::LineEntry;
use super::utils::display_width;

pub(crate) fn word_wrap(text: &str, max_width: usize) -> SmallVec<[CompactString; 4]> {
    if text.is_empty() || max_width == 0 {
        return smallvec![CompactString::from(text)];
    }
    if display_width(text) <= max_width {
        return smallvec![CompactString::from(text)];
    }

    let mut lines: SmallVec<[CompactString; 4]> = SmallVec::new();
    let mut line = String::new();
    let mut line_width: usize = 0;

    for word in text.split_inclusive(char::is_whitespace) {
        let word_trimmed = word.trim_end_matches(char::is_whitespace);
        let word_w = display_width(word);
        let trimmed_w = display_width(word_trimmed);

        if word_trimmed.is_empty() {
            if line_width > 0 && line_width < max_width {
                line.push(' ');
                line_width += 1;
            }
            continue;
        }

        if line_width + word_w <= max_width {
            line.push_str(word);
            line_width += word_w;
        } else if !line.is_empty() && line_width + 1 + trimmed_w <= max_width {
            line.push(' ');
            line.push_str(word_trimmed);
            line_width += 1 + trimmed_w;
            if word.ends_with(char::is_whitespace) {
                line.push(' ');
                line_width += 1;
            }
        } else {
            if !line.is_empty() {
                lines.push(CompactString::from(line.trim_end()));
            }
            line.clear();
            line_width = 0;

            if trimmed_w > max_width {
                for ch in word_trimmed.chars() {
                    let cw = super::utils::char_display_width(ch);
                    if line_width + cw > max_width && !line.is_empty() {
                        lines.push(CompactString::from(&line));
                        line.clear();
                        line_width = 0;
                    }
                    line.push(ch);
                    line_width += cw;
                }
            } else {
                line.push_str(word_trimmed);
                line_width += trimmed_w;
            }
            if word.ends_with(char::is_whitespace) {
                line.push(' ');
                line_width += 1;
            }
        }
    }

    let trimmed = line.trim_end();
    if !trimmed.is_empty() {
        lines.push(CompactString::from(trimmed));
    }

    if lines.is_empty() {
        lines.push(CompactString::from(text));
    }

    lines
}

fn flush_acc(acc: &str, color: Color, max_width: usize, out: &mut Vec<LineEntry>) {
    if acc.is_empty() {
        return;
    }
    for line in acc.split('\n') {
        let trimmed = line.trim_end_matches('\r');
        if trimmed.is_empty() {
            out.push(LineEntry {
                text: CompactString::new(""),
                color,
            });
        } else {
            for chunk in word_wrap(trimmed, max_width) {
                out.push(LineEntry { text: chunk, color });
            }
        }
    }
}

fn bullet_prefix(col: Color) -> &'static str {
    match col {
        Color::DarkGrey => "  ┊ ",
        _ => "  • ",
    }
}

pub fn markdown_to_styled(text: &str, max_width: usize) -> Vec<LineEntry> {
    if text.is_empty() {
        return Vec::new();
    }

    let parser = pulldown_cmark::Parser::new_ext(
        text,
        Options::ENABLE_TABLES
            | Options::ENABLE_FOOTNOTES
            | Options::ENABLE_STRIKETHROUGH
            | Options::ENABLE_TASKLISTS,
    );
    let mut result = Vec::new();
    let mut acc = String::new();

    let mut in_heading = false;
    let mut in_code_block = false;
    let mut in_blockquote = false;
    let mut ordered_list = false;
    let mut list_item_count: u64 = 0;
    let mut table_rows: Vec<Vec<String>> = Vec::new();
    let mut table_row: Vec<String> = Vec::new();
    let mut table_cell = String::new();
    let mut table_alignments: Vec<Alignment> = Vec::new();
    let mut link_url = String::new();
    let mut in_table_cell = false;

    for event in parser {
        match event {
            Event::Start(tag) => match tag {
                Tag::Paragraph => {}
                Tag::Heading { level: _, .. } => {
                    flush_acc(&acc, Color::White, max_width, &mut result);
                    acc.clear();
                    in_heading = true;
                }
                Tag::CodeBlock(_kind) => {
                    flush_acc(&acc, Color::White, max_width, &mut result);
                    acc.clear();
                    in_code_block = true;
                }
                Tag::BlockQuote(_) => {
                    flush_acc(&acc, Color::White, max_width, &mut result);
                    acc.clear();
                    in_blockquote = true;
                }
                Tag::List(t) => {
                    ordered_list = t.is_some();
                    list_item_count = 0;
                }
                Tag::Item => {
                    flush_acc(&acc, Color::White, max_width, &mut result);
                    acc.clear();
                    list_item_count += 1;
                }
                Tag::FootnoteDefinition(_) => {}
                Tag::Table(alignments) => {
                    flush_acc(&acc, Color::White, max_width, &mut result);
                    acc.clear();
                    table_rows.clear();
                    table_row.clear();
                    table_cell.clear();
                    table_alignments = alignments;
                }
                Tag::TableHead => {
                    table_rows.clear();
                }
                Tag::TableRow => {
                    table_row.clear();
                }
                Tag::TableCell => {
                    table_cell.clear();
                    in_table_cell = true;
                }
                Tag::Link {
                    link_type: _,
                    dest_url,
                    title: _,
                    id: _,
                } => {
                    link_url = dest_url.to_string();
                }
                _ => {}
            },
            Event::End(tag_end) => match tag_end {
                TagEnd::Paragraph => {
                    let color = if in_blockquote {
                        Color::DarkGrey
                    } else {
                        Color::White
                    };
                    flush_acc(&acc, color, max_width, &mut result);
                    acc.clear();
                }
                TagEnd::Heading(_) => {
                    flush_acc(&acc, Color::Cyan, max_width, &mut result);
                    acc.clear();
                    in_heading = false;
                    result.push(LineEntry {
                        text: CompactString::new(""),
                        color: Color::White,
                    });
                }
                TagEnd::CodeBlock => {
                    for line in acc.split('\n') {
                        let trimmed = line.trim_end_matches('\r');
                        if trimmed.is_empty() {
                            result.push(LineEntry {
                                text: CompactString::new(""),
                                color: Color::DarkYellow,
                            });
                        } else {
                            result.push(LineEntry {
                                text: CompactString::from(trimmed),
                                color: Color::DarkYellow,
                            });
                        }
                    }
                    acc.clear();
                    in_code_block = false;
                    result.push(LineEntry {
                        text: CompactString::new(""),
                        color: Color::White,
                    });
                }
                TagEnd::BlockQuote(_) => {
                    let mut quoted = Vec::new();
                    for line in acc.split('\n') {
                        let trimmed = line.trim_end_matches('\r');
                        if trimmed.is_empty() {
                            quoted.push(LineEntry {
                                text: CompactString::new(""),
                                color: Color::DarkGrey,
                            });
                        } else {
                            let prefixed = format!("│ {}", trimmed);
                            for chunk in word_wrap(&prefixed, max_width) {
                                quoted.push(LineEntry {
                                    text: chunk,
                                    color: Color::DarkGrey,
                                });
                            }
                        }
                    }
                    result.extend(quoted);
                    acc.clear();
                    in_blockquote = false;
                    result.push(LineEntry {
                        text: CompactString::new(""),
                        color: Color::White,
                    });
                }
                TagEnd::Item => {
                    let color = if in_blockquote {
                        Color::DarkGrey
                    } else {
                        Color::White
                    };
                    let bullet = if ordered_list {
                        format!(" {}. ", list_item_count)
                    } else {
                        bullet_prefix(color).to_string()
                    };
                    let mut item_lines = Vec::new();
                    let mut first = true;
                    for line in acc.split('\n') {
                        let trimmed = line.trim_end_matches('\r');
                        if trimmed.is_empty() {
                            item_lines.push(LineEntry {
                                text: CompactString::new(""),
                                color,
                            });
                        } else if first {
                            let prefixed = format!("{}{}", bullet, trimmed);
                            for chunk in word_wrap(&prefixed, max_width) {
                                item_lines.push(LineEntry { text: chunk, color });
                            }
                            first = false;
                        } else {
                            for chunk in word_wrap(trimmed, max_width) {
                                item_lines.push(LineEntry { text: chunk, color });
                            }
                        }
                    }
                    result.extend(item_lines);
                    acc.clear();
                }
                TagEnd::List(_) => {
                    ordered_list = false;
                    list_item_count = 0;
                    result.push(LineEntry {
                        text: CompactString::new(""),
                        color: Color::White,
                    });
                }
                TagEnd::Link => {
                    if !link_url.is_empty() {
                        if in_table_cell {
                            table_cell.push_str(&format!(" ({})", link_url));
                        } else if !acc.is_empty() {
                            flush_acc(&acc, Color::DarkCyan, max_width, &mut result);
                            let note = format!("  ↪ {}", link_url);
                            for chunk in word_wrap(&note, max_width) {
                                result.push(LineEntry {
                                    text: chunk,
                                    color: Color::DarkGrey,
                                });
                            }
                            acc.clear();
                        }
                    }
                    link_url.clear();
                }
                TagEnd::Table => {
                    flush_table(&table_rows, &table_alignments, max_width, &mut result);
                    table_rows.clear();
                    table_alignments.clear();
                    result.push(LineEntry {
                        text: CompactString::new(""),
                        color: Color::White,
                    });
                }
                TagEnd::TableHead => {
                    let cells = std::mem::take(&mut table_row);
                    let cell_text: Vec<String> =
                        cells.into_iter().map(|c| c.trim().to_string()).collect();
                    if !cell_text.iter().all(|c| c.is_empty()) {
                        table_rows.push(cell_text);
                    }
                }
                TagEnd::TableRow => {
                    let cells = std::mem::take(&mut table_row);
                    let cell_text: Vec<String> =
                        cells.into_iter().map(|c| c.trim().to_string()).collect();
                    if !cell_text.iter().all(|c| c.is_empty()) {
                        table_rows.push(cell_text);
                    }
                }
                TagEnd::TableCell => {
                    in_table_cell = false;
                    table_row.push(std::mem::take(&mut table_cell));
                }
                TagEnd::FootnoteDefinition => {}
                _ => {}
            },
            Event::Text(t) => {
                if in_table_cell {
                    table_cell.push_str(&t);
                } else {
                    acc.push_str(&t);
                }
            }
            Event::Code(t) => {
                if in_table_cell {
                    table_cell.push_str(&format!("`{}`", t));
                } else {
                    let color = if in_blockquote {
                        Color::DarkGrey
                    } else {
                        Color::White
                    };
                    flush_acc(&acc, color, max_width, &mut result);
                    acc.clear();
                    let code_text = format!("`{}`", t);
                    for chunk in word_wrap(&code_text, max_width) {
                        result.push(LineEntry {
                            text: chunk,
                            color: Color::Yellow,
                        });
                    }
                }
            }
            Event::SoftBreak | Event::HardBreak => {
                if in_table_cell {
                    table_cell.push('\n');
                } else {
                    acc.push('\n');
                }
            }
            Event::Rule => {
                flush_acc(&acc, Color::White, max_width, &mut result);
                acc.clear();
                let rule: String = "\u{2500}".repeat(max_width.min(40));
                result.push(LineEntry {
                    text: CompactString::from(rule),
                    color: Color::DarkGrey,
                });
                result.push(LineEntry {
                    text: CompactString::new(""),
                    color: Color::White,
                });
            }
            Event::Html(t) => {
                if in_table_cell {
                    table_cell.push_str(&t);
                } else {
                    acc.push_str(&t);
                }
            }
            Event::InlineHtml(t) => {
                if in_table_cell {
                    table_cell.push_str(&t);
                } else {
                    acc.push_str(&t);
                }
            }
            Event::FootnoteReference(t) => {
                acc.push_str(&t);
            }
            Event::TaskListMarker(checked) => {
                if checked {
                    acc.push_str("[x]");
                } else {
                    acc.push_str("[ ]");
                }
            }
            _ => {}
        }
    }

    if !acc.is_empty() {
        let color = if in_blockquote {
            Color::DarkGrey
        } else if in_code_block {
            Color::DarkYellow
        } else if in_heading {
            Color::Cyan
        } else {
            Color::White
        };
        flush_acc(&acc, color, max_width, &mut result);
    }

    result
}

fn flush_table(
    rows: &[Vec<String>],
    alignments: &[Alignment],
    max_width: usize,
    out: &mut Vec<LineEntry>,
) {
    if rows.is_empty() {
        return;
    }

    let col_count = rows.iter().map(|r| r.len()).max().unwrap_or(0);
    if col_count == 0 {
        return;
    }

    let mut col_widths: Vec<usize> = vec![0; col_count];
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            if i < col_count {
                col_widths[i] = col_widths[i].max(display_width(cell));
            }
        }
    }

    let available = max_width.saturating_sub((col_count + 1) * 2);
    if available <= col_count {
        return;
    }

    let total_req: usize = col_widths.iter().sum();
    if total_req > available {
        let excess: f64 = (total_req - available) as f64 / col_count as f64;
        for w in col_widths.iter_mut() {
            let reduce = (excess.ceil() as usize).min(w.saturating_sub(4));
            *w = w.saturating_sub(reduce);
        }
    }

    let top = format_table_rule(&col_widths, '\u{250c}', '\u{252c}', '\u{2510}');
    let sep = format_table_rule(&col_widths, '\u{251c}', '\u{253c}', '\u{2524}');
    let bot = format_table_rule(&col_widths, '\u{2514}', '\u{2534}', '\u{2518}');

    push_table_line(&top, Color::DarkGrey, out);
    for (i, row) in rows.iter().enumerate() {
        for line in format_table_row(row, &col_widths, alignments) {
            push_table_line(&line, Color::White, out);
        }
        if i == 0 && rows.len() > 1 {
            push_table_line(&sep, Color::DarkGrey, out);
        }
    }
    push_table_line(&bot, Color::DarkGrey, out);
}

fn format_table_rule(widths: &[usize], left: char, mid: char, right: char) -> String {
    let mut s = String::with_capacity(widths.iter().sum::<usize>() + widths.len() * 3);
    s.push(left);
    for (i, w) in widths.iter().enumerate() {
        for _ in 0..*w + 2 {
            s.push('\u{2500}');
        }
        if i + 1 < widths.len() {
            s.push(mid);
        }
    }
    s.push(right);
    s
}

fn push_table_line(text: &str, color: Color, out: &mut Vec<LineEntry>) {
    out.push(LineEntry {
        text: CompactString::from(text),
        color,
    });
}

fn format_table_row(cells: &[String], widths: &[usize], alignments: &[Alignment]) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut cell_wrapped: Vec<Vec<String>> = Vec::new();
    let mut max_subrows = 0usize;

    for (i, cell) in cells.iter().enumerate() {
        let width = widths.get(i).copied().unwrap_or(10);
        let wrapped = if display_width(cell) <= width {
            vec![cell.clone()]
        } else {
            let mut chunks = Vec::new();
            for chunk in word_wrap(cell, width) {
                chunks.push(chunk.to_string());
            }
            chunks
        };
        max_subrows = max_subrows.max(wrapped.len());
        cell_wrapped.push(wrapped);
    }

    for subrow in 0..max_subrows {
        let mut line = String::new();
        line.push('\u{2502}');
        for (i, cw) in cell_wrapped.iter().enumerate() {
            let width = widths.get(i).copied().unwrap_or(10);
            let text = cw.get(subrow).map(|s| s.as_str()).unwrap_or("");
            let text_w = display_width(text);
            let align = alignments.get(i).copied().unwrap_or(Alignment::None);
            let padding = width.saturating_sub(text_w);
            line.push(' ');
            match align {
                Alignment::Center => {
                    let left_pad = padding / 2;
                    let right_pad = padding - left_pad;
                    for _ in 0..left_pad {
                        line.push(' ');
                    }
                    line.push_str(text);
                    for _ in 0..right_pad {
                        line.push(' ');
                    }
                }
                Alignment::Right => {
                    for _ in 0..padding {
                        line.push(' ');
                    }
                    line.push_str(text);
                }
                Alignment::None | Alignment::Left => {
                    line.push_str(text);
                    for _ in 0..padding {
                        line.push(' ');
                    }
                }
            }
            line.push(' ');
            if i + 1 < cell_wrapped.len() {
                line.push('\u{2502}');
            }
        }
        line.push('\u{2502}');
        lines.push(line);
    }

    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── word_wrap ───────────────────────────────────────────────────────

    #[test]
    fn wrap_fits_within_width() {
        let result = word_wrap("hello world", 20);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], "hello world");
    }

    #[test]
    fn wrap_empty() {
        let result = word_wrap("", 10);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], "");
    }

    #[test]
    fn wrap_zero_width() {
        let result = word_wrap("hello world", 0);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], "hello world");
    }

    #[test]
    fn wrap_at_word_boundary() {
        let result = word_wrap("hello world foo bar", 12);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0], "hello world");
        assert_eq!(result[1], "foo bar");
    }

    #[test]
    fn wrap_long_single_word() {
        let result = word_wrap("supercalifragilisticexpialidocious", 10);
        assert!(result.len() > 1);
        for line in &result {
            assert!(display_width(line) <= 10);
        }
    }

    #[test]
    fn wrap_preserves_bullet() {
        let result = word_wrap("  • hello world this is a test with a longer bullet", 20);
        assert!(result[0].contains('•'));
    }

    #[test]
    fn wrap_multiple_spaces() {
        let result = word_wrap("a  b  c", 10);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], "a  b  c");
    }

    // ── markdown_to_styled: inline code ─────────────────────────────────

    #[test]
    fn inline_code_styled() {
        let styled = markdown_to_styled("Hello `code` world", 80);
        let yellow_lines: Vec<_> = styled.iter().filter(|e| e.color == Color::Yellow).collect();
        assert!(!yellow_lines.is_empty(), "inline code should be Yellow");
        assert!(
            yellow_lines[0].text.contains('`'),
            "inline code should have backticks"
        );
    }

    #[test]
    fn multiple_inline_codes_no_duplication() {
        let styled = markdown_to_styled("foo `a` bar `b` baz", 80);
        let joined: String = styled
            .iter()
            .map(|e| e.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        // "foo" should appear exactly once
        assert_eq!(
            joined.matches("foo").count(),
            1,
            "prose before first code must not duplicate: {joined}"
        );
        // "bar" between codes should appear exactly once
        assert_eq!(
            joined.matches("bar").count(),
            1,
            "prose between codes must not duplicate: {joined}"
        );
        assert_eq!(
            joined.matches("baz").count(),
            1,
            "prose after last code must not duplicate: {joined}"
        );
    }


    #[test]
    fn inline_code_in_blockquote() {
        let styled = markdown_to_styled("> Some `code` here", 80);
        let yellow_lines: Vec<_> = styled.iter().filter(|e| e.color == Color::Yellow).collect();
        assert!(!yellow_lines.is_empty());
    }

    // ── markdown_to_styled: links ───────────────────────────────────────

    #[test]
    fn link_renders_url() {
        let styled = markdown_to_styled("Click [here](https://example.com) for more", 80);
        let has_url = styled
            .iter()
            .any(|e| e.text.contains("https://example.com"));
        assert!(has_url, "link URL should appear in output");
    }

    #[test]
    fn link_text_is_colored() {
        let styled = markdown_to_styled("[link text](https://x.com)", 80);
        let cyan_lines: Vec<_> = styled
            .iter()
            .filter(|e| e.color == Color::DarkCyan)
            .collect();
        assert!(!cyan_lines.is_empty(), "link text should be DarkCyan");
    }

    #[test]
    fn link_url_is_dark_grey() {
        let styled = markdown_to_styled("[text](https://x.com)", 80);
        let url_lines: Vec<_> = styled
            .iter()
            .filter(|e| e.color == Color::DarkGrey && e.text.contains('\u{21aa}'))
            .collect();
        assert!(
            !url_lines.is_empty(),
            "link URL should be DarkGrey with arrow"
        );
    }

    // ── markdown_to_styled: tables ──────────────────────────────────────

    #[test]
    fn table_renders_borders() {
        let input = "| A | B |\n|---|---|\n| 1 | 2 |\n";
        let styled = markdown_to_styled(input, 80);
        let text: Vec<&str> = styled.iter().map(|e| e.text.as_str()).collect();
        let joined = text.join("");
        assert!(
            joined.contains('\u{250c}'),
            "table should have top-left border"
        );
        assert!(
            joined.contains('\u{2510}'),
            "table should have top-right border"
        );
        assert!(
            joined.contains('\u{2514}'),
            "table should have bottom-left border"
        );
        assert!(
            joined.contains('\u{2502}'),
            "table should have vertical separators"
        );
    }

    #[test]
    fn table_contains_content() {
        let input = "| Name | Age |\n|------|-----|\n| Alice | 30 |\n";
        let styled = markdown_to_styled(input, 80);
        let joined: String = styled
            .iter()
            .map(|e| e.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            joined.contains("Name"),
            "table should contain header 'Name'"
        );
        assert!(
            joined.contains("Alice"),
            "table should contain data 'Alice'"
        );
        assert!(joined.contains("30"), "table should contain data '30'");
    }

    #[test]
    fn table_borders_are_dark_grey() {
        let input = "| X |\n|---|\n| y |\n";
        let styled = markdown_to_styled(input, 80);
        let border_lines: Vec<_> = styled
            .iter()
            .filter(|e| e.color == Color::DarkGrey && e.text.contains('\u{2500}'))
            .collect();
        assert!(!border_lines.is_empty(), "table borders should be DarkGrey");
    }

    #[test]
    fn table_content_is_white() {
        let input = "| X |\n|---|\n| y |\n";
        let styled = markdown_to_styled(input, 80);
        let content_lines: Vec<_> = styled
            .iter()
            .filter(|e| e.color == Color::White && e.text.contains('y'))
            .collect();
        assert!(!content_lines.is_empty(), "table content should be White");
    }

    #[test]
    fn table_blank_skipped() {
        markdown_to_styled("||\n|--|\n||\n", 80);
    }

    #[test]
    fn table_with_inline_code() {
        let input = "| Cmd | Desc |\n|-----|------|\n| `ls` | list |\n";
        let styled = markdown_to_styled(input, 80);
        let joined: String = styled
            .iter()
            .map(|e| e.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            joined.contains("`ls`"),
            "table should contain inline code `ls`"
        );
    }

    #[test]
    fn table_with_alignment() {
        let input = "| L | C | R |\n|:--|:-:|--:|\n| a | b | c |\n";
        let styled = markdown_to_styled(input, 80);
        assert!(!styled.is_empty(), "aligned table should render");
    }

    // ── markdown_to_styled: regression ──────────────────────────────────

    #[test]
    fn empty_input_returns_empty_vec() {
        let styled = markdown_to_styled("", 80);
        assert!(styled.is_empty());
    }

    #[test]
    fn headings_still_work() {
        let styled = markdown_to_styled("# Hello", 80);
        let heading = styled.iter().find(|e| e.color == Color::Cyan);
        assert!(heading.is_some(), "heading should be Cyan");
        assert!(heading.unwrap().text.contains("Hello"));
    }

    #[test]
    fn code_blocks_still_work() {
        let input = "```\nlet x = 1;\n```\n";
        let styled = markdown_to_styled(input, 80);
        let code_lines: Vec<_> = styled
            .iter()
            .filter(|e| e.color == Color::DarkYellow)
            .collect();
        assert!(!code_lines.is_empty(), "code block should be DarkYellow");
    }

    #[test]
    fn lists_still_work() {
        let styled = markdown_to_styled("- item one\n- item two\n", 80);
        let bullets = styled.iter().filter(|e| e.text.contains('\u{2022}'));
        assert_eq!(bullets.count(), 2, "unordered list should have two bullets");
    }

    #[test]
    fn blockquotes_still_work() {
        let styled = markdown_to_styled("> quoted text", 80);
        let quoted = styled.iter().any(|e| e.color == Color::DarkGrey);
        assert!(quoted, "blockquote text should be DarkGrey");
    }
}
