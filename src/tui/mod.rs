use anyhow::Result;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph},
    Frame, Terminal,
};
use std::io;
use tokio::sync::mpsc;

use crate::protocol::PlainMessage;

pub struct ChatUI {
    messages: Vec<PlainMessage>,
    input: String,
    status: String,
    peer_id: Option<String>,
    own_id: String,
}

impl ChatUI {
    pub fn new(own_id: String) -> Self {
        Self {
            messages: Vec::new(),
            input: String::new(),
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
        // Setup terminal
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;

        let result = self.run_loop(&mut terminal, &mut msg_tx, &mut incoming_rx, &mut status_rx).await;

        // Restore terminal
        disable_raw_mode()?;
        execute!(
            terminal.backend_mut(),
            LeaveAlternateScreen,
            DisableMouseCapture
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
                                return Ok(());
                            }
                            KeyCode::Char(c) => {
                                self.input.push(c);
                            }
                            KeyCode::Backspace => {
                                self.input.pop();
                            }
                            KeyCode::Enter => {
                                if !self.input.is_empty() {
                                    let msg = PlainMessage::new(
                                        self.own_id.clone(),
                                        self.input.clone(),
                                    );
                                    self.messages.push(msg.clone());
                                    let _ = msg_tx.send(msg);
                                    self.input.clear();
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
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(1),
                Constraint::Length(3),
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
        let messages: Vec<ListItem> = self
            .messages
            .iter()
            .map(|m| {
                let timestamp = chrono::DateTime::from_timestamp(m.timestamp, 0)
                    .map(|dt| dt.format("%H:%M:%S").to_string())
                    .unwrap_or_else(|| "??:??:??".to_string());
                
                let is_own = m.sender == self.own_id;
                let sender_short = &m.sender[..8.min(m.sender.len())];
                
                let line = Line::from(vec![
                    Span::styled(
                        format!("[{}] ", timestamp),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::styled(
                        format!("{}: ", sender_short),
                        Style::default().fg(if is_own { Color::Cyan } else { Color::Magenta }),
                    ),
                    Span::raw(&m.content),
                ]);
                
                ListItem::new(line)
            })
            .collect();

        let messages_widget = List::new(messages)
            .block(Block::default().borders(Borders::ALL).title("Messages"));
        f.render_widget(messages_widget, chunks[1]);

        // Input
        let input = Paragraph::new(self.input.as_str())
            .style(Style::default().fg(Color::White))
            .block(Block::default().borders(Borders::ALL).title("Type message (Ctrl+C to quit)"));
        f.render_widget(input, chunks[2]);
    }
}
