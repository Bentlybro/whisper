use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph, Wrap},
    Frame, Terminal,
};
use std::collections::HashMap;
use std::io;
use tokio::sync::mpsc;

use crate::client::{OutgoingMessage, PeerInfo};
use crate::protocol::PlainMessage;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Tab {
    Global,
    DirectMessage(String), // peer_id
}

pub struct ChatUI {
    tabs: Vec<Tab>,
    active_tab: usize,
    messages: HashMap<Tab, Vec<PlainMessage>>,
    input: Vec<char>,
    cursor: usize,
    status: String,
    peers: HashMap<String, PeerInfo>,
    own_id: String,
    own_nickname: Option<String>,
}

impl ChatUI {
    pub fn new(own_id: String, nickname: Option<String>) -> Self {
        let mut messages = HashMap::new();
        messages.insert(Tab::Global, Vec::new());
        
        Self {
            tabs: vec![Tab::Global],
            active_tab: 0,
            messages,
            input: Vec::new(),
            cursor: 0,
            status: "Connecting...".to_string(),
            peers: HashMap::new(),
            own_id,
            own_nickname: nickname,
        }
    }

    pub async fn run(
        &mut self,
        mut msg_tx: mpsc::UnboundedSender<OutgoingMessage>,
        mut incoming_rx: mpsc::UnboundedReceiver<PlainMessage>,
        mut status_rx: mpsc::UnboundedReceiver<String>,
        mut peer_update_rx: mpsc::UnboundedReceiver<HashMap<String, PeerInfo>>,
    ) -> Result<()> {
        // Setup terminal - no mouse capture so native text selection works
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen)?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;

        let result = self.run_loop(&mut terminal, &mut msg_tx, &mut incoming_rx, &mut status_rx, &mut peer_update_rx).await;

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
        msg_tx: &mut mpsc::UnboundedSender<OutgoingMessage>,
        incoming_rx: &mut mpsc::UnboundedReceiver<PlainMessage>,
        status_rx: &mut mpsc::UnboundedReceiver<String>,
        peer_update_rx: &mut mpsc::UnboundedReceiver<HashMap<String, PeerInfo>>,
    ) -> Result<()> {
        loop {
            terminal.draw(|f| self.ui(f))?;

            // Handle events with timeout
            if event::poll(std::time::Duration::from_millis(100))? {
                if let Event::Key(key) = event::read()? {
                    if key.kind == KeyEventKind::Press {
                        match key.code {
                            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                // Send leave notification on global chat
                                let nick = self.display_name();
                                let leave_msg = PlainMessage::system(
                                    self.own_id.clone(),
                                    format!("{} has left", nick),
                                );
                                let _ = msg_tx.send(OutgoingMessage::Global(leave_msg));
                                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                                return Ok(());
                            }
                            KeyCode::Tab => {
                                self.next_tab();
                            }
                            KeyCode::Left if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                self.prev_tab();
                            }
                            KeyCode::Right if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                self.next_tab();
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
                            KeyCode::Enter if key.modifiers.contains(event::KeyModifiers::SHIFT) 
                                || key.modifiers.contains(event::KeyModifiers::ALT) => {
                                // Shift+Enter or Alt+Enter inserts a newline
                                self.input.insert(self.cursor, '\n');
                                self.cursor += 1;
                            }
                            KeyCode::Enter => {
                                if !self.input.is_empty() {
                                    let text: String = self.input.iter().collect();
                                    self.handle_input(text, msg_tx);
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
                if msg.dm_request {
                    // Peer wants to open a DM — create the tab silently
                    let sender_id = msg.sender.clone();
                    let dm_tab = Tab::DirectMessage(sender_id.clone());
                    if !self.tabs.contains(&dm_tab) {
                        self.tabs.push(dm_tab.clone());
                        self.messages.insert(dm_tab, Vec::new());
                        let peer_name = self.get_peer_display_name(&sender_id);
                        self.status = format!("{} opened a DM with you", peer_name);
                    }
                } else if msg.system && !msg.content.is_empty() {
                    // System messages (join/leave) go to global chat
                    self.messages.entry(Tab::Global).or_insert_with(Vec::new).push(msg);
                } else if !msg.system {
                    let sender_id = msg.sender.clone();
                    
                    if msg.direct {
                        // Direct message — auto-create DM tab if needed
                        let dm_tab = Tab::DirectMessage(sender_id.clone());
                        if !self.tabs.contains(&dm_tab) {
                            self.tabs.push(dm_tab.clone());
                            self.messages.insert(dm_tab.clone(), Vec::new());
                        }
                        self.messages.entry(dm_tab).or_insert_with(Vec::new).push(msg);
                    } else {
                        // Global message — put in existing DM tab if open, otherwise global
                        let dm_tab = Tab::DirectMessage(sender_id.clone());
                        if self.tabs.contains(&dm_tab) {
                            // If we have a DM tab, still put global messages in global
                            self.messages.entry(Tab::Global).or_insert_with(Vec::new).push(msg);
                        } else {
                            self.messages.entry(Tab::Global).or_insert_with(Vec::new).push(msg);
                        }
                    }
                }
            }

            // Check for status updates
            while let Ok(status) = status_rx.try_recv() {
                self.status = status;
            }

            // Check for peer updates
            while let Ok(peers) = peer_update_rx.try_recv() {
                self.peers = peers;
            }
        }
    }

    fn handle_input(&mut self, text: String, msg_tx: &mut mpsc::UnboundedSender<OutgoingMessage>) {
        // Handle commands
        if text.starts_with('/') {
            let parts: Vec<&str> = text[1..].split_whitespace().collect();
            if parts.is_empty() {
                return;
            }

            match parts[0] {
                "dm" => {
                    if parts.len() < 2 {
                        self.status = "Usage: /dm <nickname|peer_id>".to_string();
                        return;
                    }
                    let target = parts[1];
                    self.open_dm_tab(target, Some(msg_tx));
                }
                "nick" => {
                    if parts.len() < 2 {
                        self.status = "Usage: /nick <new_nickname>".to_string();
                        return;
                    }
                    let new_nick = parts[1..].join(" ");
                    self.own_nickname = Some(new_nick.clone());
                    
                    // Send nickname update to all peers
                    for peer_id in self.peers.keys() {
                        let nickname_msg = PlainMessage::nickname(
                            self.own_id.clone(),
                            new_nick.clone(),
                        );
                        let _ = msg_tx.send(OutgoingMessage::Direct {
                            target_id: peer_id.clone(),
                            message: nickname_msg,
                        });
                    }
                    
                    self.status = format!("Nickname changed to: {}", new_nick);
                }
                _ => {
                    self.status = format!("Unknown command: /{}", parts[0]);
                }
            }
            return;
        }

        // Regular message
        let current_tab = &self.tabs[self.active_tab];
        
        match current_tab {
            Tab::Global => {
                let msg = PlainMessage::new(self.own_id.clone(), text);
                // Add to our own view
                self.messages.entry(Tab::Global).or_insert_with(Vec::new).push(msg.clone());
                // Send to all peers
                let _ = msg_tx.send(OutgoingMessage::Global(msg));
            }
            Tab::DirectMessage(peer_id) => {
                let msg = PlainMessage::direct(self.own_id.clone(), text);
                // Add to our own DM view
                self.messages.entry(current_tab.clone()).or_insert_with(Vec::new).push(msg.clone());
                // Send to specific peer
                let _ = msg_tx.send(OutgoingMessage::Direct {
                    target_id: peer_id.clone(),
                    message: msg,
                });
            }
        }
    }

    fn open_dm_tab(&mut self, target: &str, msg_tx: Option<&mpsc::UnboundedSender<OutgoingMessage>>) {
        // Try to find peer by nickname or ID prefix
        let peer_id = self.find_peer_by_name_or_id(target);
        
        if let Some(id) = peer_id {
            let dm_tab = Tab::DirectMessage(id.clone());
            
            // Check if tab already exists
            if let Some(idx) = self.tabs.iter().position(|t| t == &dm_tab) {
                self.active_tab = idx;
            } else {
                // Create new tab
                self.tabs.push(dm_tab.clone());
                self.messages.insert(dm_tab, Vec::new());
                self.active_tab = self.tabs.len() - 1;
                
                // Send DM request to peer so they open a tab too
                if let Some(tx) = msg_tx {
                    let dm_req = PlainMessage::dm_request(self.own_id.clone());
                    let _ = tx.send(OutgoingMessage::Direct {
                        target_id: id.clone(),
                        message: dm_req,
                    });
                }
            }
            
            let peer_name = self.get_peer_display_name(&id);
            self.status = format!("Opened DM with {}", peer_name);
        } else {
            self.status = format!("Peer not found: {}", target);
        }
    }

    fn find_peer_by_name_or_id(&self, target: &str) -> Option<String> {
        // First try exact nickname match
        for (id, info) in &self.peers {
            if let Some(ref nick) = info.nickname {
                if nick.eq_ignore_ascii_case(target) {
                    return Some(id.clone());
                }
            }
        }
        
        // Then try ID prefix match
        for id in self.peers.keys() {
            if id.starts_with(target) {
                return Some(id.clone());
            }
        }
        
        None
    }

    fn get_peer_display_name(&self, peer_id: &str) -> String {
        if let Some(info) = self.peers.get(peer_id) {
            if let Some(ref nick) = info.nickname {
                return nick.clone();
            }
        }
        peer_id[..12.min(peer_id.len())].to_string()
    }

    fn display_name(&self) -> String {
        self.own_nickname.clone().unwrap_or_else(|| self.own_id[..12].to_string())
    }

    fn next_tab(&mut self) {
        if !self.tabs.is_empty() {
            self.active_tab = (self.active_tab + 1) % self.tabs.len();
        }
    }

    fn prev_tab(&mut self) {
        if !self.tabs.is_empty() {
            self.active_tab = if self.active_tab == 0 {
                self.tabs.len() - 1
            } else {
                self.active_tab - 1
            };
        }
    }

    /// Count display lines for input text (accounting for newlines and wrapping)
    fn count_input_lines(input: &[char], inner_width: usize) -> usize {
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
    fn cursor_position(input: &[char], cursor: usize, inner_width: usize) -> (u16, u16) {
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

    fn ui(&self, f: &mut Frame) {
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
                Constraint::Length(3),
                Constraint::Min(1),
                Constraint::Length(input_height),
                Constraint::Length(3),
            ])
            .split(left_side);

        // Header
        let nick_display = self.own_nickname.as_deref().unwrap_or("No nickname");
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
                Span::styled(nick_display, Style::default().fg(Color::Magenta)),
                Span::raw(" | "),
                Span::raw(&self.status),
            ]),
        ])
        .block(Block::default().borders(Borders::ALL).title("Status"));
        f.render_widget(header, left_chunks[0]);

        // Messages
        self.render_messages(f, left_chunks[1]);

        // Input
        let input_text: String = self.input.iter().collect();
        let current_tab_name = self.get_tab_name(&self.tabs[self.active_tab]);
        let input_title = format!("Type message in {} (Ctrl+C quit, Tab switch)", current_tab_name);
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

    fn render_messages(&self, f: &mut Frame, area: Rect) {
        let current_tab = &self.tabs[self.active_tab];
        let messages = self.messages.get(current_tab).map(|v| v.as_slice()).unwrap_or(&[]);
        
        let msg_inner_width = if area.width > 2 { area.width - 2 } else { 1 };
        let msg_inner_height = if area.height > 2 { area.height - 2 } else { 0 };

        let mut msg_lines: Vec<Line> = Vec::new();
        for m in messages {
            if m.system && m.nickname.is_none() {
                // Join/leave system messages
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

    fn render_tabs(&self, f: &mut Frame, area: Rect) {
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

    fn render_sidebar(&self, f: &mut Frame, area: Rect) {
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

    fn get_tab_name(&self, tab: &Tab) -> String {
        match tab {
            Tab::Global => "#global".to_string(),
            Tab::DirectMessage(peer_id) => {
                let name = self.get_peer_display_name(peer_id);
                name
            }
        }
    }
}
