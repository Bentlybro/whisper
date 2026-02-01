use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame, Terminal,
};
use std::io;
use tokio::sync::mpsc;

use crate::protocol::PlainMessage;

pub struct ChatUI {
    messages: Vec<PlainMessage>,
    input: Vec<char>,
    cursor: usize,
    status: String,
    peer_id: Option<String>,
    own_id: String,
}

impl ChatUI {
    pub fn new(own_id: String) -> Self {
        Self {
            messages: Vec::new(),
            input: Vec::new(),
            cursor: 0,
            status: "Connecting...".to_string(),
            peer_id: None,
            own_id,
        }
    }

    pub async fn run(
        &mut self,
        mut msg_tx: mpsc::UnboundedSender<PlainMessage>,
        mut incoming_rx: mpsc::UnboundedReceiver<PlainMessage>,
        mut status_rx: mpsc::UnboundedReceiver<String>,
    ) -> Result<()> {
        // Setup terminal - no mouse capture so native text selection works
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen)?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;

        let result = self.run_loop(&mut terminal, &mut msg_tx, &mut incoming_rx, &mut status_rx).await;

        // Restore terminal
        disable_raw_mode()?;
        execute!(
            terminal.backend_mut(),
            LeaveAlternateScreen,
        )?;
        terminal.show_cursor()?;

        result
    }

    async fn run_loop(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
        msg_tx: &mut mpsc::UnboundedSender<PlainMessage>,
        incoming_rx: &mut mpsc::UnboundedReceiver<PlainMessage>,
        status_rx: &mut mpsc::UnboundedReceiver<String>,
    ) -> Result<()> {
        loop {
            terminal.draw(|f| self.ui(f))?;

            // Handle events with timeout
            if event::poll(std::time::Duration::from_millis(100))? {
                if let Event::Key(key) = event::read()? {
                    if key.kind == KeyEventKind::Press {
                        match key.code {
                            KeyCode::Char('c') if key.modifiers.contains(event::KeyModifiers::CONTROL) => {
                                // Send leave notification
                                let leave_msg = PlainMessage::system(
                                    self.own_id.clone(),
                                    format!("{} has left", &self.own_id[..12.min(self.own_id.len())]),
                                );
                                let _ = msg_tx.send(leave_msg);
                                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                                return Ok(());
                            }
                            KeyCode::Char(c) => {
                                self.input.insert(self.cursor, c);
                                self.cursor += 1;
                            }
                            KeyCode::Backspace => {
                                if self.cursor > 0 {
                                    self.cursor -= 1;
                                    self.input.remove(self.cursor);
                                }
                            }
                            KeyCode::Delete => {
                                if self.cursor < self.input.len() {
                                    self.input.remove(self.cursor);
                                }
                            }
                            KeyCode::Left => {
                                if self.cursor > 0 {
                                    self.cursor -= 1;
                                }
                            }
                            KeyCode::Right => {
                                if self.cursor < self.input.len() {
                                    self.cursor += 1;
                                }
                            }
                            KeyCode::Home => {
                                self.cursor = 0;
                            }
                            KeyCode::End => {
                                self.cursor = self.input.len();
                            }
                            KeyCode::Enter => {
                                if !self.input.is_empty() {
                                    let text: String = self.input.iter().collect();
                                    let msg = PlainMessage::new(
                                        self.own_id.clone(),
                                        text,
                                    );
                                    self.messages.push(msg.clone());
                                    let _ = msg_tx.send(msg);
                                    self.input.clear();
                                    self.cursor = 0;
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }

            // Check for incoming messages
            while let Ok(msg) = incoming_rx.try_recv() {
                self.messages.push(msg);
            }

            // Check for status updates
            while let Ok(status) = status_rx.try_recv() {
                self.status = status;
            }
        }
    }

    fn ui(&self, f: &mut Frame) {
        // Calculate input box height based on text wrapping
        let total_width = f.area().width as usize;
        let inner_width = if total_width > 2 { total_width - 2 } else { 1 };
        let input_len = self.input.len();
        let input_lines = if input_len == 0 {
            1
        } else {
            (input_len + inner_width - 1) / inner_width
        };
        let input_height = (input_lines as u16) + 2; // +2 for borders

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(1),
                Constraint::Length(input_height),
            ])
            .split(f.area());

        // Header
        let header = Paragraph::new(vec![
            Line::from(vec![
                Span::styled("🔒 WSP", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                Span::raw(" | "),
                Span::styled("E2EE Chat", Style::default().fg(Color::Green)),
            ]),
            Line::from(vec![
                Span::raw("Your ID: "),
                Span::styled(&self.own_id[..16.min(self.own_id.len())], Style::default().fg(Color::Yellow)),
                Span::raw(" | "),
                Span::raw(&self.status),
            ]),
        ])
        .block(Block::default().borders(Borders::ALL).title("Status"));
        f.render_widget(header, chunks[0]);

        // Messages
        let msg_area = chunks[1];
        let msg_inner_width = if msg_area.width > 2 { msg_area.width - 2 } else { 1 };
        let msg_inner_height = if msg_area.height > 2 { msg_area.height - 2 } else { 0 };

        let mut msg_lines: Vec<Line> = Vec::new();
        for m in &self.messages {
            if m.system {
                let text = format!("[{}]", m.content);
                let padding = (msg_inner_width as usize).saturating_sub(text.len()) / 2;
                let padded = format!("{}{}", " ".repeat(padding), text);
                msg_lines.push(Line::from(Span::styled(
                    padded,
                    Style::default().fg(Color::Yellow).add_modifier(Modifier::ITALIC),
                )));
                continue;
            }

            let timestamp = chrono::DateTime::from_timestamp(m.timestamp, 0)
                .map(|dt| dt.format("%H:%M:%S").to_string())
                .unwrap_or_else(|| "??:??:??".to_string());

            let is_own = m.sender == self.own_id;
            let sender_short = &m.sender[..8.min(m.sender.len())];
            let prefix = format!("[{}] {}: ", timestamp, sender_short);
            let prefix_style = if is_own { Color::Cyan } else { Color::Magenta };

            let content = &m.content;
            let available = (msg_inner_width as usize).saturating_sub(prefix.len());

            if available == 0 || content.is_empty() {
                msg_lines.push(Line::from(vec![
                    Span::styled(format!("[{}] ", timestamp), Style::default().fg(Color::DarkGray)),
                    Span::styled(format!("{}: ", sender_short), Style::default().fg(prefix_style)),
                    Span::raw(content),
                ]));
            } else {
                let mut chars: Vec<char> = content.chars().collect();
                let mut first = true;
                let indent = " ".repeat(prefix.len());

                while !chars.is_empty() {
                    let take = if first { available } else { (msg_inner_width as usize).saturating_sub(indent.len()) };
                    let chunk_len = take.min(chars.len());
                    let chunk: String = chars.drain(..chunk_len).collect();

                    if first {
                        msg_lines.push(Line::from(vec![
                            Span::styled(format!("[{}] ", timestamp), Style::default().fg(Color::DarkGray)),
                            Span::styled(format!("{}: ", sender_short), Style::default().fg(prefix_style)),
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
        f.render_widget(messages_widget, msg_area);

        // Input with text wrapping and visible cursor
        let input_text: String = self.input.iter().collect();
        let input = Paragraph::new(input_text)
            .style(Style::default().fg(Color::White))
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title("Type message (Ctrl+C to quit)"));
        f.render_widget(input, chunks[2]);

        // Position the cursor in the input box
        let cursor_x = (self.cursor % inner_width) as u16;
        let cursor_y = (self.cursor / inner_width) as u16;
        f.set_cursor_position((
            chunks[2].x + 1 + cursor_x, // +1 for border
            chunks[2].y + 1 + cursor_y,  // +1 for border
        ));
    }
}
