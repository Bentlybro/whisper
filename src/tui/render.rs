use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph, Wrap},
    Frame,
};

use super::helpers::format_duration;
use super::types::{CallType, Tab};
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

    pub(crate) fn ui(&self, f: &mut Frame) {
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

        let left_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(4),  // Header needs 4: border + 2 content lines + border
                Constraint::Min(1),
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
        f.render_widget(input, left_chunks[2]);

        // Position cursor
        let (cursor_x, cursor_y) = Self::cursor_position(&self.input, self.cursor, inner_width);
        f.set_cursor_position((
            left_chunks[2].x + 1 + cursor_x,
            left_chunks[2].y + 1 + cursor_y,
        ));

        // Tabs bar
        self.render_tabs(f, left_chunks[3]);

        // Sidebar with online peers
        self.render_sidebar(f, sidebar);
    }

    pub(crate) fn render_messages(&self, f: &mut Frame, area: Rect) {
        let current_tab = &self.tabs[self.active_tab];
        let messages = self.messages.get(current_tab).map(|v| v.as_slice()).unwrap_or(&[]);

        let msg_inner_width = if area.width > 2 { area.width - 2 } else { 1 };
        let msg_inner_height = if area.height > 2 { area.height - 2 } else { 0 };

        let mut msg_lines: Vec<Line> = Vec::new();
        for m in messages {
            if m.system && m.nickname.is_none() {
                // Join/leave/system messages
                let text = format!("[{}]", m.content);
                let padding = (msg_inner_width as usize).saturating_sub(text.len()) / 2;
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

            let prefix = format!("[{}] {}: ", timestamp, sender_display);
            let prefix_style = if is_own { Color::Cyan } else { Color::Magenta };

            let content = &m.content;
            let available = (msg_inner_width as usize).saturating_sub(prefix.len());
            let indent = " ".repeat(prefix.len());

            if available == 0 || content.is_empty() {
                msg_lines.push(Line::from(vec![
                    Span::styled(format!("[{}] ", timestamp), Style::default().fg(Color::DarkGray)),
                    Span::styled(format!("{}: ", sender_display), Style::default().fg(prefix_style)),
                    Span::raw(content),
                ]));
            } else {
                // Split content by newlines first, then wrap each line
                let content_lines: Vec<&str> = content.split('\n').collect();
                let mut first = true;

                for line in content_lines {
                    let mut chars: Vec<char> = line.chars().collect();

                    // Handle empty lines (from consecutive newlines)
                    if chars.is_empty() {
                        if first {
                            msg_lines.push(Line::from(vec![
                                Span::styled(format!("[{}] ", timestamp), Style::default().fg(Color::DarkGray)),
                                Span::styled(format!("{}: ", sender_display), Style::default().fg(prefix_style)),
                            ]));
                            first = false;
                        } else {
                            msg_lines.push(Line::from(Span::raw(indent.clone())));
                        }
                        continue;
                    }

                    while !chars.is_empty() {
                        let take = if first {
                            available
                        } else {
                            (msg_inner_width as usize).saturating_sub(indent.len())
                        };
                        let chunk_len = take.min(chars.len());
                        let chunk: String = chars.drain(..chunk_len).collect();

                        if first {
                            msg_lines.push(Line::from(vec![
                                Span::styled(format!("[{}] ", timestamp), Style::default().fg(Color::DarkGray)),
                                Span::styled(format!("{}: ", sender_display), Style::default().fg(prefix_style)),
                                Span::raw(chunk),
                            ]));
                            first = false;
                        } else {
                            msg_lines.push(Line::from(vec![
                                Span::raw(indent.clone()),
                                Span::raw(chunk),
                            ]));
                        }
                    }
                }
            }
        }

        // Auto-scroll
        let total_lines = msg_lines.len() as u16;
        let scroll_offset = if total_lines > msg_inner_height {
            total_lines - msg_inner_height
        } else {
            0
        };

        let messages_widget = Paragraph::new(msg_lines)
            .scroll((scroll_offset, 0))
            .block(Block::default().borders(Borders::ALL).title("Messages"));
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
            let display = if let Some(ref nick) = info.nickname {
                format!("● {}", nick)
            } else {
                format!("● {}", &id[..12.min(id.len())])
            };
            ListItem::new(display).style(Style::default().fg(Color::Green))
        }).collect();

        if peer_items.is_empty() {
            peer_items.push(ListItem::new("(no peers)").style(Style::default().fg(Color::DarkGray)));
        }

        let list = List::new(peer_items)
            .block(Block::default().borders(Borders::ALL).title(format!("Online ({})", self.peers.len())));
        f.render_widget(list, area);
    }
}
