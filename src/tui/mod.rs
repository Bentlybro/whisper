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
use std::path::PathBuf;
use tokio::sync::mpsc;

use crate::client::{OutgoingMessage, PeerInfo};
use crate::protocol::{PlainMessage, FileOffer, FileChunk};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Tab {
    Global,
    DirectMessage(String), // peer_id
}

#[derive(Clone, Debug)]
struct PendingFileOffer {
    offer: FileOffer,
    from_peer: String,
    tab: Tab,
}

#[derive(Clone, Debug)]
struct ActiveTransfer {
    offer: FileOffer,
    chunks_received: Vec<Option<Vec<u8>>>,
    save_path: PathBuf,
    chunks_done: u32,
}

#[derive(Clone, Debug)]
struct OutgoingTransfer {
    offer: FileOffer,
    file_data: Vec<u8>,
    target_peer: String,
    chunks_sent: u32,
    is_direct: bool,
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
    pending_offers: HashMap<String, PendingFileOffer>,
    active_transfers: HashMap<String, ActiveTransfer>,
    outgoing_transfers: HashMap<String, OutgoingTransfer>,
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
            pending_offers: HashMap::new(),
            active_transfers: HashMap::new(),
            outgoing_transfers: HashMap::new(),
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
                // Handle file-related messages
                if msg.file_offer.is_some() {
                    self.handle_file_offer(msg.clone());
                    continue;
                } else if msg.file_chunk.is_some() {
                    self.handle_file_chunk(msg.clone());
                    continue;
                } else if let Some(accept) = msg.file_response {
                    self.handle_file_response(msg.clone(), accept, msg_tx);
                    continue;
                }
                
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
        let trimmed = text.trim();
        if trimmed.starts_with('/') {
            let parts: Vec<&str> = trimmed[1..].split_whitespace().collect();
            if parts.is_empty() {
                self.status = "Empty command".to_string();
                return;
            }
            
            self.status = format!("Command: /{} ({})", parts[0], parts.len());

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
                "send" | "share" => {
                    if parts.len() < 2 {
                        self.status = "Usage: /send <filepath>".to_string();
                        return;
                    }
                    let filepath = parts[1..].join(" ");
                    self.handle_share_command(&filepath, msg_tx);
                }
                "accept" => {
                    let save_path = if parts.len() >= 2 {
                        parts[1..].join(" ")
                    } else {
                        ".".to_string()
                    };
                    self.handle_accept_command(&save_path, msg_tx);
                }
                "reject" => {
                    self.handle_reject_command(msg_tx);
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
                Span::styled("🔒 WSP v2", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
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

    // File sharing methods
    
    fn handle_share_command(&mut self, filepath: &str, msg_tx: &mut mpsc::UnboundedSender<OutgoingMessage>) {
        const CHUNK_SIZE: usize = 16384; // 16KB chunks for reliable WebSocket transfer
        
        self.status = format!("Reading file: {}...", filepath);
        
        // Check if we have any peers
        if self.peers.is_empty() {
            self.status = "No peers connected to share with".to_string();
            return;
        }
        
        // Expand tilde in path (Unix) or use as-is (Windows handles C:\ etc)
        let path = if filepath.starts_with("~/") {
            if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
                PathBuf::from(home).join(&filepath[2..])
            } else {
                PathBuf::from(filepath)
            }
        } else {
            PathBuf::from(filepath)
        };
        
        // Read file
        let file_data = match std::fs::read(&path) {
            Ok(data) => data,
            Err(e) => {
                self.status = format!("Failed to read file: {}", e);
                return;
            }
        };
        
        // Get filename only (no path)
        let filename = match path.file_name() {
            Some(name) => name.to_string_lossy().to_string(),
            None => {
                self.status = "Invalid file path".to_string();
                return;
            }
        };
        
        // Compute Blake3 hash
        let checksum = blake3::hash(&file_data).to_hex().to_string();
        
        // Calculate chunks
        let total_chunks = ((file_data.len() + CHUNK_SIZE - 1) / CHUNK_SIZE) as u32;
        
        // Generate random file ID
        let file_id = format!("{:x}", rand::random::<u64>());
        
        let offer = FileOffer {
            file_id: file_id.clone(),
            filename: filename.clone(),
            size: file_data.len() as u64,
            checksum,
            total_chunks,
        };
        
        // Determine if this is a direct message or global
        let current_tab = &self.tabs[self.active_tab];
        let (is_direct, target_peer) = match current_tab {
            Tab::Global => (false, String::new()),
            Tab::DirectMessage(peer_id) => (true, peer_id.clone()),
        };
        
        // Send offer
        let offer_msg = PlainMessage::file_offer(self.own_id.clone(), offer.clone(), is_direct);
        
        if is_direct {
            let _ = msg_tx.send(OutgoingMessage::Direct {
                target_id: target_peer.clone(),
                message: offer_msg,
            });
        } else {
            let _ = msg_tx.send(OutgoingMessage::Global(offer_msg));
        }
        
        // Store outgoing transfer
        self.outgoing_transfers.insert(file_id.clone(), OutgoingTransfer {
            offer: offer.clone(),
            file_data,
            target_peer,
            chunks_sent: 0,
            is_direct,
        });
        
        self.status = format!("Offering file: {} ({})", filename, Self::format_size(offer.size));
    }
    
    fn handle_accept_command(&mut self, save_path: &str, msg_tx: &mut mpsc::UnboundedSender<OutgoingMessage>) {
        // Find pending offer for current tab
        let current_tab = &self.tabs[self.active_tab];
        
        let offer_to_accept = self.pending_offers.iter()
            .find(|(_, pending)| &pending.tab == current_tab)
            .map(|(id, pending)| (id.clone(), pending.clone()));
        
        if let Some((file_id, pending)) = offer_to_accept {
            // Expand path
            let save_dir = if save_path.starts_with("~/") {
                if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
                    PathBuf::from(home).join(&save_path[2..])
                } else {
                    PathBuf::from(save_path)
                }
            } else {
                PathBuf::from(save_path)
            };
            
            // Create full path
            let full_path = if save_dir.is_dir() || save_path.ends_with('/') || save_path == "." {
                save_dir.join(&pending.offer.filename)
            } else {
                save_dir
            };
            
            // Send acceptance
            let response_msg = PlainMessage::file_response(
                self.own_id.clone(),
                file_id.clone(),
                true,
                pending.tab != Tab::Global,
            );
            
            match &pending.tab {
                Tab::Global => {
                    let _ = msg_tx.send(OutgoingMessage::Global(response_msg));
                }
                Tab::DirectMessage(peer_id) => {
                    let _ = msg_tx.send(OutgoingMessage::Direct {
                        target_id: peer_id.clone(),
                        message: response_msg,
                    });
                }
            }
            
            // Prepare to receive
            let chunks_vec = vec![None; pending.offer.total_chunks as usize];
            self.active_transfers.insert(file_id.clone(), ActiveTransfer {
                offer: pending.offer.clone(),
                chunks_received: chunks_vec,
                save_path: full_path.clone(),
                chunks_done: 0,
            });
            
            self.pending_offers.remove(&file_id);
            self.status = format!("Accepting {}, saving to {}", pending.offer.filename, full_path.display());
        } else {
            self.status = "No pending file offer in this tab".to_string();
        }
    }
    
    fn handle_reject_command(&mut self, msg_tx: &mut mpsc::UnboundedSender<OutgoingMessage>) {
        let current_tab = &self.tabs[self.active_tab];
        
        let offer_to_reject = self.pending_offers.iter()
            .find(|(_, pending)| &pending.tab == current_tab)
            .map(|(id, pending)| (id.clone(), pending.clone()));
        
        if let Some((file_id, pending)) = offer_to_reject {
            // Send rejection
            let response_msg = PlainMessage::file_response(
                self.own_id.clone(),
                file_id.clone(),
                false,
                pending.tab != Tab::Global,
            );
            
            match &pending.tab {
                Tab::Global => {
                    let _ = msg_tx.send(OutgoingMessage::Global(response_msg));
                }
                Tab::DirectMessage(peer_id) => {
                    let _ = msg_tx.send(OutgoingMessage::Direct {
                        target_id: peer_id.clone(),
                        message: response_msg,
                    });
                }
            }
            
            self.pending_offers.remove(&file_id);
            self.status = format!("Rejected file: {}", pending.offer.filename);
        } else {
            self.status = "No pending file offer in this tab".to_string();
        }
    }
    
    fn handle_file_offer(&mut self, msg: PlainMessage) {
        if let Some(offer) = msg.file_offer {
            let file_id = offer.file_id.clone();
            let sender_name = self.get_peer_display_name(&msg.sender);
            
            // Determine which tab this belongs to
            let tab = if msg.direct {
                Tab::DirectMessage(msg.sender.clone())
            } else {
                Tab::Global
            };
            
            // Store pending offer
            self.pending_offers.insert(file_id, PendingFileOffer {
                offer: offer.clone(),
                from_peer: msg.sender,
                tab,
            });
            
            self.status = format!(
                "{} wants to share {} ({}) — /accept [path] or /reject",
                sender_name,
                offer.filename,
                Self::format_size(offer.size)
            );
        }
    }
    
    fn handle_file_chunk(&mut self, msg: PlainMessage) {
        if let Some(chunk) = msg.file_chunk {
            let file_id = &chunk.file_id;
            
            if let Some(transfer) = self.active_transfers.get_mut(file_id) {
                // Store chunk
                if (chunk.index as usize) < transfer.chunks_received.len() {
                    if transfer.chunks_received[chunk.index as usize].is_none() {
                        transfer.chunks_received[chunk.index as usize] = Some(chunk.data);
                        transfer.chunks_done += 1;
                        
                        let progress = (transfer.chunks_done as f64 / transfer.offer.total_chunks as f64) * 100.0;
                        self.status = format!(
                            "Receiving {}: {:.0}% ({}/{})",
                            transfer.offer.filename,
                            progress,
                            transfer.chunks_done,
                            transfer.offer.total_chunks
                        );
                        
                        // Check if complete
                        if transfer.chunks_done == transfer.offer.total_chunks {
                            self.finalize_transfer(file_id);
                        }
                    }
                }
            }
        }
    }
    
    fn handle_file_response(&mut self, msg: PlainMessage, accept: bool, msg_tx: &mut mpsc::UnboundedSender<OutgoingMessage>) {
        const CHUNK_SIZE: usize = 65536;
        let file_id = &msg.content;
        
        if !accept {
            // File was rejected
            if let Some(transfer) = self.outgoing_transfers.remove(file_id) {
                self.status = format!("File rejected: {}", transfer.offer.filename);
            }
            return;
        }
        
        // File was accepted - start sending chunks
        let sender_name = self.get_peer_display_name(&msg.sender);
        if let Some(transfer) = self.outgoing_transfers.get_mut(file_id) {
            self.status = format!("{} accepted {}. Sending...", sender_name, transfer.offer.filename);
            
            // Send all chunks
            let total_chunks = transfer.offer.total_chunks as usize;
            for i in 0..total_chunks {
                let start = i * CHUNK_SIZE;
                let end = ((i + 1) * CHUNK_SIZE).min(transfer.file_data.len());
                let chunk_data = transfer.file_data[start..end].to_vec();
                
                let chunk = FileChunk {
                    file_id: file_id.clone(),
                    index: i as u32,
                    data: chunk_data,
                };
                
                let chunk_msg = PlainMessage::file_chunk(
                    self.own_id.clone(),
                    chunk,
                    transfer.is_direct,
                );
                
                if transfer.is_direct {
                    let _ = msg_tx.send(OutgoingMessage::Direct {
                        target_id: transfer.target_peer.clone(),
                        message: chunk_msg,
                    });
                } else {
                    let _ = msg_tx.send(OutgoingMessage::Global(chunk_msg));
                }
                
                transfer.chunks_sent += 1;
            }
            
            let filename = transfer.offer.filename.clone();
            self.outgoing_transfers.remove(file_id);
            self.status = format!("Sent {} successfully", filename);
        }
    }
    
    fn finalize_transfer(&mut self, file_id: &str) {
        if let Some(transfer) = self.active_transfers.remove(file_id) {
            // Reassemble file
            let mut file_data = Vec::new();
            for chunk_opt in &transfer.chunks_received {
                if let Some(chunk) = chunk_opt {
                    file_data.extend_from_slice(chunk);
                } else {
                    self.status = format!("Error: Missing chunks for {}", transfer.offer.filename);
                    return;
                }
            }
            
            // Verify checksum
            let actual_checksum = blake3::hash(&file_data).to_hex().to_string();
            if actual_checksum != transfer.offer.checksum {
                self.status = format!("Error: Checksum mismatch for {}", transfer.offer.filename);
                return;
            }
            
            // Write file
            if let Err(e) = std::fs::write(&transfer.save_path, &file_data) {
                self.status = format!("Error saving file: {}", e);
                return;
            }
            
            self.status = format!(
                "File saved: {} ✓ ({})",
                transfer.save_path.display(),
                Self::format_size(transfer.offer.size)
            );
        }
    }
    
    fn format_size(bytes: u64) -> String {
        const KB: u64 = 1024;
        const MB: u64 = KB * 1024;
        const GB: u64 = MB * 1024;
        
        if bytes >= GB {
            format!("{:.2} GB", bytes as f64 / GB as f64)
        } else if bytes >= MB {
            format!("{:.2} MB", bytes as f64 / MB as f64)
        } else if bytes >= KB {
            format!("{:.2} KB", bytes as f64 / KB as f64)
        } else {
            format!("{} bytes", bytes)
        }
    }
}
