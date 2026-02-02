use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap},
    Frame,
};

use ratatui_image::StatefulImage;

use super::helpers::format_duration;
use super::types::{CallType, ReadStatus, Tab};
use super::ChatUI;

impl ChatUI {
    /// Count display lines for input text (accounting for newlines and wrapping)
    pub(crate) fn count_input_lines(input: &[char], inner_width: usize) -> usize {
        if input.is_empty() {
            return 1;
        }
        let mut lines = 0;
        let mut col = 0;
        for &ch in input {
            if ch == '\n' {
                lines += 1;
                col = 0;
            } else {
                col += 1;
                if col > inner_width {
                    lines += 1;
                    col = 1;
                }
            }
        }
        lines + 1 // +1 because we count breaks, not lines
    }

    /// Calculate cursor (x, y) position accounting for newlines and wrapping
    pub(crate) fn cursor_position(input: &[char], cursor: usize, inner_width: usize) -> (u16, u16) {
        let mut row: u16 = 0;
        let mut col: u16 = 0;
        for i in 0..cursor.min(input.len()) {
            if input[i] == '\n' {
                row += 1;
                col = 0;
            } else {
                col += 1;
                if col as usize > inner_width {
                    row += 1;
                    col = 1;
                }
            }
        }
        (col, row)
    }

    /// Parse markdown-lite text into styled spans
    /// Supports: **bold**, *italic*, `code`
    fn parse_markdown(text: &str) -> Vec<Span<'static>> {
        let mut spans: Vec<Span<'static>> = Vec::new();
        let chars: Vec<char> = text.chars().collect();
        let len = chars.len();
        let mut i = 0;
        let mut current = String::new();

        while i < len {
            // Check for **bold**
            if i + 1 < len && chars[i] == '*' && chars[i + 1] == '*' {
                // Flush current text
                if !current.is_empty() {
                    spans.push(Span::raw(current.clone()));
                    current.clear();
                }
                // Find closing **
                let start = i + 2;
                let mut end = None;
                let mut j = start;
                while j + 1 < len {
                    if chars[j] == '*' && chars[j + 1] == '*' {
                        end = Some(j);
                        break;
                    }
                    j += 1;
                }
                if let Some(end_pos) = end {
                    let bold_text: String = chars[start..end_pos].iter().collect();
                    spans.push(Span::styled(bold_text, Style::default().add_modifier(Modifier::BOLD)));
                    i = end_pos + 2;
                } else {
                    current.push(chars[i]);
                    i += 1;
                }
            }
            // Check for *italic* (but not **)
            else if chars[i] == '*' && (i + 1 >= len || chars[i + 1] != '*') {
                // Flush current text
                if !current.is_empty() {
                    spans.push(Span::raw(current.clone()));
                    current.clear();
                }
                let start = i + 1;
                let mut end = None;
                let mut j = start;
                while j < len {
                    if chars[j] == '*' && (j + 1 >= len || chars[j + 1] != '*') {
                        end = Some(j);
                        break;
                    }
                    j += 1;
                }
                if let Some(end_pos) = end {
                    let italic_text: String = chars[start..end_pos].iter().collect();
                    spans.push(Span::styled(italic_text, Style::default().add_modifier(Modifier::ITALIC)));
                    i = end_pos + 1;
                } else {
                    current.push(chars[i]);
                    i += 1;
                }
            }
            // Check for `code`
            else if chars[i] == '`' {
                // Flush current text
                if !current.is_empty() {
                    spans.push(Span::raw(current.clone()));
                    current.clear();
                }
                let start = i + 1;
                let mut end = None;
                let mut j = start;
                while j < len {
                    if chars[j] == '`' {
                        end = Some(j);
                        break;
                    }
                    j += 1;
                }
                if let Some(end_pos) = end {
                    let code_text: String = chars[start..end_pos].iter().collect();
                    spans.push(Span::styled(code_text, Style::default().fg(Color::Yellow).bg(Color::DarkGray)));
                    i = end_pos + 1;
                } else {
                    current.push(chars[i]);
                    i += 1;
                }
            } else {
                current.push(chars[i]);
                i += 1;
            }
        }

        if !current.is_empty() {
            spans.push(Span::raw(current));
        }

        if spans.is_empty() {
            spans.push(Span::raw(String::new()));
        }

        spans
    }

    /// Word-wrap text to fit within a given width, returning wrapped lines
    fn word_wrap(text: &str, max_width: usize) -> Vec<String> {
        if max_width == 0 {
            return vec![text.to_string()];
        }

        let mut lines = Vec::new();
        for line in text.split('\n') {
            if line.is_empty() {
                lines.push(String::new());
                continue;
            }

            let words: Vec<&str> = line.split_whitespace().collect();
            if words.is_empty() {
                lines.push(String::new());
                continue;
            }

            let mut current_line = String::new();
            for word in words {
                if current_line.is_empty() {
                    if word.len() > max_width {
                        // Break long words
                        let mut remaining = word;
                        while remaining.len() > max_width {
                            let (chunk, rest) = remaining.split_at(max_width);
                            lines.push(chunk.to_string());
                            remaining = rest;
                        }
                        current_line = remaining.to_string();
                    } else {
                        current_line = word.to_string();
                    }
                } else if current_line.len() + 1 + word.len() <= max_width {
                    current_line.push(' ');
                    current_line.push_str(word);
                } else {
                    lines.push(current_line);
                    if word.len() > max_width {
                        let mut remaining = word;
                        while remaining.len() > max_width {
                            let (chunk, rest) = remaining.split_at(max_width);
                            lines.push(chunk.to_string());
                            remaining = rest;
                        }
                        current_line = remaining.to_string();
                    } else {
                        current_line = word.to_string();
                    }
                }
            }
            if !current_line.is_empty() {
                lines.push(current_line);
            }
        }

        if lines.is_empty() {
            lines.push(String::new());
        }

        lines
    }

    /// Get typing indicator text for the current tab
    fn get_typing_text(&self) -> Option<String> {
        let current_tab = &self.tabs[self.active_tab];
        let typing_names: Vec<String> = self.typing_peers.iter()
            .filter(|(peer_id, _)| {
                match current_tab {
                    Tab::Global => true,
                    Tab::DirectMessage(dm_peer) => *peer_id == dm_peer,
                    Tab::Group(group_id) => {
                        self.groups.get(group_id)
                            .map(|g| g.members.contains(peer_id))
                            .unwrap_or(false)
                    }
                }
            })
            .map(|(peer_id, _)| self.get_peer_display_name(peer_id))
            .collect();

        if typing_names.is_empty() {
            None
        } else if typing_names.len() == 1 {
            Some(format!("{} is typing...", typing_names[0]))
        } else if typing_names.len() == 2 {
            Some(format!("{} and {} are typing...", typing_names[0], typing_names[1]))
        } else {
            Some(format!("{} people are typing...", typing_names.len()))
        }
    }

    /// Render the screen share view — frame takes up most of the terminal
    fn render_screen_share_view(&mut self, f: &mut Frame) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(1),   // Frame area
                Constraint::Length(3), // Input area
                Constraint::Length(1), // Status bar
            ])
            .split(f.area());

        let frame_area = chunks[0];
        let input_area = chunks[1];
        let status_area = chunks[2];

        // Determine role label
        let role = if self.screen_share_target.is_some() {
            let peer_name = self.screen_share_target.as_ref()
                .map(|id| self.get_peer_display_name(id))
                .unwrap_or_else(|| "unknown".to_string());
            format!("📤 Sharing your screen with {}", peer_name)
        } else if self.screen_viewer_from.is_some() {
            let peer_name = self.screen_viewer_from.as_ref()
                .map(|id| self.get_peer_display_name(id))
                .unwrap_or_else(|| "unknown".to_string());
            format!("📥 Viewing {}'s screen", peer_name)
        } else {
            "Screen Share".to_string()
        };

        // Render the frame using ratatui-image (Sixel/Kitty/iTerm2/halfblocks)
        if self.screen_protocol.is_some() {
            // Render image DIRECTLY in the frame area — no block/border
            // (borders cause ratatui to clear the area, which flickers with Sixel/Kitty)
            let image_widget = StatefulImage::default();
            if let Some(ref mut protocol) = self.screen_protocol {
                f.render_stateful_widget(image_widget, frame_area, protocol);
            }
        } else {
            // No frame yet — show waiting message centered
            let waiting = Paragraph::new(format!("🖥️  {} — waiting for frames...", role))
                .style(Style::default().fg(Color::DarkGray))
                .alignment(ratatui::layout::Alignment::Center);
            f.render_widget(waiting, frame_area);
        }

        // Input box — so user can type /stop-share without leaving screen view
        let input_text: String = self.input.iter().collect();
        let input_widget = Paragraph::new(input_text)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::DarkGray))
                    .title(Span::styled(
                        " Type here (Esc=chat) ",
                        Style::default().fg(Color::DarkGray),
                    )),
            )
            .style(Style::default().fg(Color::White));
        f.render_widget(input_widget, input_area);

        // Place cursor in input area
        let inner_w = if input_area.width > 2 { input_area.width as usize - 2 } else { 1 };
        let (cx, cy) = Self::cursor_position(&self.input, self.cursor, inner_w);
        f.set_cursor_position((input_area.x + 1 + cx, input_area.y + 1 + cy));

        // Status bar — includes role info + controls
        let status = Paragraph::new(Line::from(vec![
            Span::styled(
                format!(" 🖥️  {} ", role),
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ),
            Span::raw("│ "),
            Span::styled("Esc", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::raw(" chat │ "),
            Span::styled("F5", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::raw(" toggle │ "),
            Span::styled(
                if let Some(ref frame) = self.screen_frame {
                    format!("{}×{}", frame.width, frame.height)
                } else {
                    "—".to_string()
                },
                Style::default().fg(Color::DarkGray),
            ),
        ]))
        .style(Style::default().bg(Color::DarkGray).fg(Color::White));
        f.render_widget(status, status_area);
    }

    pub(crate) fn ui(&mut self, f: &mut Frame) {
        // If screen share view is active, render the frame full-screen with a small status bar
        if self.screen_view_active {
            self.render_screen_share_view(f);
            return;
        }

        let main_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Min(20),
                Constraint::Length(20),
            ])
            .split(f.area());

        let left_side = main_chunks[0];
        let sidebar = main_chunks[1];

        // Calculate input box height based on text wrapping and newlines
        let total_width = left_side.width as usize;
        let inner_width = if total_width > 2 { total_width - 2 } else { 1 };
        let input_lines = Self::count_input_lines(&self.input, inner_width);
        let input_height = (input_lines as u16) + 2; // +2 for borders

        // Check for typing indicator
        let typing_text = self.get_typing_text();
        let typing_height: u16 = if typing_text.is_some() { 1 } else { 0 };

        let left_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(4),  // Header needs 4: border + 2 content lines + border
                Constraint::Min(1),
                Constraint::Length(typing_height),
                Constraint::Length(input_height),
                Constraint::Length(3),
            ])
            .split(left_side);

        // Header
        let nick_display = self.own_nickname.as_deref().unwrap_or("No nickname");
        let mut header_line2 = vec![
            Span::raw("Your ID: "),
            Span::styled(&self.own_id[..16.min(self.own_id.len())], Style::default().fg(Color::Yellow)),
            Span::raw(" | "),
            Span::styled(nick_display, Style::default().fg(Color::Magenta)),
        ];

        if let Some(ref call) = self.active_call {
            let call_label = match &call.call_type {
                CallType::Direct(peer_id) => self.get_peer_display_name(peer_id),
                CallType::Group { group_id } => self.group_name(group_id),
            };
            let duration = chrono::Utc::now() - call.start_time;
            let duration_str = format_duration(duration);
            let mute_icon = if call.muted { "🔇" } else { "🔊" };
            let mute_hint = if call.muted { " [MUTED]" } else { "" };
            header_line2.push(Span::raw(" | "));
            header_line2.push(Span::styled(
                format!("{} {} ({}){}", mute_icon, call_label, duration_str, mute_hint),
                Style::default().fg(if call.muted { Color::Red } else { Color::Green }).add_modifier(Modifier::BOLD),
            ));
        }

        header_line2.push(Span::raw(" | "));
        header_line2.push(Span::raw(&self.status));

        let header = Paragraph::new(vec![
            Line::from(vec![
                Span::styled("🔒 WSP v2", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                Span::raw(" | "),
                Span::styled("E2EE Chat", Style::default().fg(Color::Green)),
            ]),
            Line::from(header_line2),
        ])
        .block(Block::default().borders(Borders::ALL).title("Status"));
        f.render_widget(header, left_chunks[0]);

        // Messages
        self.render_messages(f, left_chunks[1]);

        // Typing indicator
        if let Some(ref typing) = typing_text {
            let typing_widget = Paragraph::new(Line::from(Span::styled(
                format!(" ✍ {}", typing),
                Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC),
            )));
            f.render_widget(typing_widget, left_chunks[2]);
        }

        // Input
        let input_text: String = self.input.iter().collect();
        let current_tab_name = self.get_tab_name(&self.tabs[self.active_tab]);
        let input_title = if self.active_call.is_some() {
            let mute_status = if self.active_call.as_ref().map(|c| c.muted).unwrap_or(false) {
                "🔇 MUTED"
            } else {
                "🎤 LIVE"
            };
            format!("{} | {} | /mute /hangup", current_tab_name, mute_status)
        } else {
            format!("Type message in {} (Ctrl+C quit, Tab switch)", current_tab_name)
        };
        let input = Paragraph::new(input_text)
            .style(Style::default().fg(Color::White))
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title(input_title));
        f.render_widget(input, left_chunks[3]);

        // Position cursor
        let (cursor_x, cursor_y) = Self::cursor_position(&self.input, self.cursor, inner_width);
        f.set_cursor_position((
            left_chunks[3].x + 1 + cursor_x,
            left_chunks[3].y + 1 + cursor_y,
        ));

        // Tabs bar
        self.render_tabs(f, left_chunks[4]);

        // Sidebar with online peers
        self.render_sidebar(f, sidebar);

        // Render autocomplete popup overlay (on top of everything)
        if let Some(ref ac) = self.autocomplete {
            self.render_autocomplete(f, ac, left_chunks[3]);
        }
    }

    pub(crate) fn render_messages(&self, f: &mut Frame, area: Rect) {
        let current_tab = &self.tabs[self.active_tab];
        let messages = self.messages.get(current_tab).map(|v| v.as_slice()).unwrap_or(&[]);

        let msg_inner_width = if area.width > 2 { (area.width - 2) as usize } else { 1 };
        let msg_inner_height = if area.height > 2 { (area.height - 2) as usize } else { 0 };

        let mut msg_lines: Vec<Line> = Vec::new();
        for m in messages {
            if m.system && m.nickname.is_none() {
                // Join/leave/system messages
                let text = format!("[{}]", m.content);
                let padding = msg_inner_width.saturating_sub(text.len()) / 2;
                let padded = format!("{}{}", " ".repeat(padding), text);
                msg_lines.push(Line::from(Span::styled(
                    padded,
                    Style::default().fg(Color::Yellow).add_modifier(Modifier::ITALIC),
                )));
                continue;
            }

            if m.system {
                // Skip nickname system messages (internal only)
                continue;
            }

            let timestamp = chrono::DateTime::from_timestamp(m.timestamp, 0)
                .map(|dt| dt.format("%H:%M:%S").to_string())
                .unwrap_or_else(|| "??:??:??".to_string());

            let is_own = m.sender == self.own_id;
            let sender_display = if is_own {
                self.display_name()
            } else {
                self.get_peer_display_name(&m.sender)
            };

            // Read receipt indicator
            let receipt_indicator = if is_own {
                if let Some(ref msg_id) = m.message_id {
                    match self.read_status.get(msg_id) {
                        Some(ReadStatus::Read) => " ✓✓",
                        Some(ReadStatus::Sent) => " ✓",
                        None => " ✓", // sent but no status tracked yet
                    }
                } else {
                    ""
                }
            } else {
                ""
            };

            let prefix = format!("[{}] {}: ", timestamp, sender_display);
            let prefix_style = if is_own { Color::Cyan } else { Color::Magenta };

            let content = &m.content;
            let available = msg_inner_width.saturating_sub(prefix.len());
            let indent = " ".repeat(prefix.len());

            if available == 0 || content.is_empty() {
                let mut spans = vec![
                    Span::styled(format!("[{}] ", timestamp), Style::default().fg(Color::DarkGray)),
                    Span::styled(format!("{}: ", sender_display), Style::default().fg(prefix_style)),
                    Span::raw(content.to_string()),
                ];
                if !receipt_indicator.is_empty() {
                    spans.push(Span::styled(receipt_indicator.to_string(), Style::default().fg(Color::Green)));
                }
                msg_lines.push(Line::from(spans));
            } else {
                // Word-wrap content, then parse markdown on each wrapped line
                let wrapped_lines = Self::word_wrap(content, available);
                let mut first = true;

                for (line_idx, line) in wrapped_lines.iter().enumerate() {
                    let is_last = line_idx == wrapped_lines.len() - 1;

                    if first {
                        let mut spans = vec![
                            Span::styled(format!("[{}] ", timestamp), Style::default().fg(Color::DarkGray)),
                            Span::styled(format!("{}: ", sender_display), Style::default().fg(prefix_style)),
                        ];
                        spans.extend(Self::parse_markdown(line));
                        if is_last && !receipt_indicator.is_empty() {
                            spans.push(Span::styled(receipt_indicator.to_string(), Style::default().fg(Color::Green)));
                        }
                        msg_lines.push(Line::from(spans));
                        first = false;
                    } else {
                        let mut spans = vec![Span::raw(indent.clone())];
                        spans.extend(Self::parse_markdown(line));
                        if is_last && !receipt_indicator.is_empty() {
                            spans.push(Span::styled(receipt_indicator.to_string(), Style::default().fg(Color::Green)));
                        }
                        msg_lines.push(Line::from(spans));
                    }
                }
            }
        }

        // Calculate scroll position
        let total_lines = msg_lines.len();
        let user_scroll = self.scroll_offset.get(current_tab).copied().unwrap_or(0);

        // Clamp user scroll to valid range
        let max_scroll = total_lines.saturating_sub(msg_inner_height);
        let clamped_scroll = user_scroll.min(max_scroll);

        let scroll_offset = if total_lines > msg_inner_height {
            (max_scroll - clamped_scroll) as u16
        } else {
            0
        };

        // Build title with scroll indicator
        let title = if clamped_scroll > 0 {
            format!("Messages [↑ {} more]", clamped_scroll)
        } else {
            "Messages".to_string()
        };

        let messages_widget = Paragraph::new(msg_lines)
            .scroll((scroll_offset, 0))
            .block(Block::default().borders(Borders::ALL).title(title));
        f.render_widget(messages_widget, area);
    }

    pub(crate) fn render_tabs(&self, f: &mut Frame, area: Rect) {
        let tab_names: Vec<String> = self.tabs.iter().enumerate().map(|(i, tab)| {
            let name = self.get_tab_name(tab);
            if i == self.active_tab {
                format!("[{}]", name)
            } else {
                format!(" {} ", name)
            }
        }).collect();

        let tabs_text = tab_names.join(" ");
        let tabs = Paragraph::new(tabs_text)
            .style(Style::default().fg(Color::White))
            .block(Block::default().borders(Borders::ALL).title("Tabs"));
        f.render_widget(tabs, area);
    }

    pub(crate) fn render_sidebar(&self, f: &mut Frame, area: Rect) {
        let mut peer_items: Vec<ListItem> = self.peers.iter().map(|(id, info)| {
            let verified_icon = if self.verified_peers.contains(id) { "✅" } else { "❓" };
            let typing_icon = if self.typing_peers.contains_key(id) { " ✍" } else { "" };
            let display = if let Some(ref nick) = info.nickname {
                format!("{} ● {}{}", verified_icon, nick, typing_icon)
            } else {
                format!("{} ● {}{}", verified_icon, &id[..12.min(id.len())], typing_icon)
            };
            let color = if self.verified_peers.contains(id) { Color::Green } else { Color::Yellow };
            ListItem::new(display).style(Style::default().fg(color))
        }).collect();

        if peer_items.is_empty() {
            peer_items.push(ListItem::new("(no peers)").style(Style::default().fg(Color::DarkGray)));
        }

        let list = List::new(peer_items)
            .block(Block::default().borders(Borders::ALL).title(format!("Online ({})", self.peers.len())));
        f.render_widget(list, area);
    }

    /// Render autocomplete popup above the input box
    fn render_autocomplete(&self, f: &mut Frame, ac: &super::types::AutocompleteState, input_area: Rect) {
        let visible_count = ac.filtered.len().min(8) as u16;
        let popup_height = visible_count + 2; // +2 for borders
        let popup_width = 45u16.min(input_area.width);

        // Position above the input box
        let popup_y = input_area.y.saturating_sub(popup_height);
        let popup_area = Rect::new(input_area.x, popup_y, popup_width, popup_height);

        // Clear the area first
        f.render_widget(Clear, popup_area);

        let items: Vec<ListItem> = ac.filtered.iter().enumerate().map(|(i, &cmd_idx)| {
            let cmd = &ac.commands[cmd_idx];
            let text = format!("/{:<16} {}", cmd.name, cmd.description);
            let style = if i == ac.selected {
                Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };
            ListItem::new(text).style(style)
        }).collect();

        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title("Commands")
                .style(Style::default().fg(Color::Cyan)));
        f.render_widget(list, popup_area);
    }
}
