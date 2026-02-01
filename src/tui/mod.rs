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

use crate::audio::AudioPipeline;
use crate::client::{OutgoingMessage, PeerInfo};
use crate::protocol::{PlainMessage, FileOffer, FileChunk, GroupInvite};

const FILE_CHUNK_SIZE: usize = 16384; // 16KB chunks for file transfer

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Tab {
    Global,
    DirectMessage(String), // peer_id
    Group(String),         // group_id
}

#[derive(Clone, Debug)]
struct GroupInfo {
    name: String,
    members: Vec<String>, // session_ids of members (excluding self)
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

#[derive(Clone, Debug)]
struct CallState {
    peer_id: String,
    start_time: chrono::DateTime<chrono::Utc>,
    muted: bool,
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
    groups: HashMap<String, GroupInfo>, // group_id -> GroupInfo
    // Voice call state
    active_call: Option<CallState>,
    pending_call_from: Option<String>, // peer_id of incoming call request
    audio_pipeline: Option<AudioPipeline>,
    audio_capture_rx: Option<mpsc::UnboundedReceiver<Vec<u8>>>, // Opus frames from mic
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
            groups: HashMap::new(),
            active_call: None,
            pending_call_from: None,
            audio_pipeline: None,
            audio_capture_rx: None,
        }
    }

    pub async fn run(
        &mut self,
        mut msg_tx: mpsc::UnboundedSender<OutgoingMessage>,
        mut incoming_rx: mpsc::UnboundedReceiver<PlainMessage>,
        mut status_rx: mpsc::UnboundedReceiver<String>,
        mut peer_update_rx: mpsc::UnboundedReceiver<HashMap<String, PeerInfo>>,
        mut audio_in_rx: mpsc::UnboundedReceiver<(String, Vec<u8>)>,
    ) -> Result<()> {
        // Setup terminal - no mouse capture so native text selection works
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen)?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;

        let result = self.run_loop(&mut terminal, &mut msg_tx, &mut incoming_rx, &mut status_rx, &mut peer_update_rx, &mut audio_in_rx).await;

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
        audio_in_rx: &mut mpsc::UnboundedReceiver<(String, Vec<u8>)>,
    ) -> Result<()> {
        // Opus decoder for incoming audio (created lazily when call starts)
        let mut opus_decoder: Option<audiopus::coder::Decoder> = None;
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
                            KeyCode::Enter if key.modifiers.contains(event::KeyModifiers::SHIFT) => {
                                // Shift+Enter inserts a newline
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
                // Handle voice call signaling
                if msg.call_request == Some(true) {
                    self.handle_incoming_call_request(&msg, msg_tx);
                    continue;
                }
                if let Some(accept) = msg.call_accept {
                    self.handle_call_response(&msg, accept, msg_tx);
                    continue;
                }
                if msg.call_hangup == Some(true) {
                    self.handle_remote_hangup(&msg);
                    continue;
                }

                // Handle group invites
                if let Some(ref invite) = msg.group_invite {
                    self.handle_group_invite(msg.clone(), invite.clone(), msg_tx);
                    continue;
                }

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

                // Handle group messages
                if let Some(ref group_id) = msg.group_id {
                    let group_tab = Tab::Group(group_id.clone());
                    // Auto-create group tab if needed (shouldn't happen normally, but safety)
                    if !self.tabs.contains(&group_tab) {
                        self.tabs.push(group_tab.clone());
                        self.messages.insert(group_tab.clone(), Vec::new());
                    }
                    self.messages.entry(group_tab).or_insert_with(Vec::new).push(msg);
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
                        // Global message
                        self.messages.entry(Tab::Global).or_insert_with(Vec::new).push(msg);
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

            // Handle incoming audio frames (decrypt → decode → playback)
            while let Ok((from, opus_data)) = audio_in_rx.try_recv() {
                if let Some(ref call) = self.active_call {
                    if call.peer_id == from {
                        // Decode and send to playback
                        if opus_decoder.is_none() {
                            opus_decoder = audiopus::coder::Decoder::new(
                                audiopus::SampleRate::Hz48000,
                                audiopus::Channels::Mono,
                            ).ok();
                        }
                        if let Some(ref mut decoder) = opus_decoder {
                            if let Ok(pcm) = AudioPipeline::decode_opus_frame(decoder, &opus_data) {
                                if let Some(ref pipeline) = self.audio_pipeline {
                                    if let Some(tx) = pipeline.playback_tx() {
                                        let _ = tx.send(pcm);
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // Send captured audio frames to peer (unless muted)
            if let Some(ref call) = self.active_call {
                let peer_id = call.peer_id.clone();
                let is_muted = call.muted;
                if let Some(ref mut capture_rx) = self.audio_capture_rx {
                    while let Ok(opus_frame) = capture_rx.try_recv() {
                        if !is_muted {
                            let _ = msg_tx.send(OutgoingMessage::Audio {
                                target_id: peer_id.clone(),
                                data: opus_frame,
                            });
                        }
                        // When muted, drain frames but don't send
                    }
                }
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
                "group" => {
                    self.handle_group_command(&parts[1..], msg_tx);
                }
                "call" => {
                    self.handle_call_command(msg_tx);
                }
                "accept-call" => {
                    self.handle_accept_call_command(msg_tx);
                }
                "reject-call" => {
                    self.handle_reject_call_command(msg_tx);
                }
                "hangup" | "end-call" => {
                    self.handle_hangup_command(msg_tx);
                }
                "mute" => {
                    if let Some(ref mut call) = self.active_call {
                        call.muted = !call.muted;
                        if call.muted {
                            self.status = "🔇 Microphone muted".to_string();
                        } else {
                            self.status = "🔊 Microphone unmuted".to_string();
                        }
                    } else {
                        self.status = "Not in a call".to_string();
                    }
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
        let current_tab = &self.tabs[self.active_tab].clone();
        
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
            Tab::Group(group_id) => {
                if let Some(group) = self.groups.get(group_id) {
                    let msg = PlainMessage::group(self.own_id.clone(), text, group_id.clone());
                    // Add to our own group view
                    self.messages.entry(current_tab.clone()).or_insert_with(Vec::new).push(msg.clone());
                    // Fan out to all group members
                    let member_ids: Vec<String> = group.members.clone();
                    let _ = msg_tx.send(OutgoingMessage::Group {
                        group_id: group_id.clone(),
                        member_ids,
                        message: msg,
                    });
                } else {
                    self.status = "Group not found".to_string();
                }
            }
        }
    }

    fn handle_group_command(&mut self, parts: &[&str], msg_tx: &mut mpsc::UnboundedSender<OutgoingMessage>) {
        if parts.is_empty() {
            self.status = "Usage: /group create <name> | invite <peer> | leave | members".to_string();
            return;
        }

        match parts[0] {
            "create" => {
                if parts.len() < 2 {
                    self.status = "Usage: /group create <name>".to_string();
                    return;
                }
                let group_name = parts[1..].join(" ");
                let group_id = generate_group_id();
                
                // Create the group locally
                self.groups.insert(group_id.clone(), GroupInfo {
                    name: group_name.clone(),
                    members: Vec::new(),
                });

                // Create the tab
                let group_tab = Tab::Group(group_id.clone());
                self.tabs.push(group_tab.clone());
                self.messages.insert(group_tab.clone(), Vec::new());
                self.active_tab = self.tabs.len() - 1;

                // Tell relay to join this room
                let _ = msg_tx.send(OutgoingMessage::JoinRoom { group_id: group_id.clone() });

                // Add system message
                let sys_msg = PlainMessage::system(
                    self.own_id.clone(),
                    format!("Group \"{}\" created. Use /group invite <peer> to add members.", group_name),
                );
                self.messages.entry(group_tab).or_insert_with(Vec::new).push(sys_msg);

                self.status = format!("Created group: {} ({})", group_name, &group_id[..8]);
            }
            "invite" => {
                if parts.len() < 2 {
                    self.status = "Usage: /group invite <nickname|peer_id>".to_string();
                    return;
                }
                let target = parts[1];

                // Must be in a group tab
                let current_tab = self.tabs[self.active_tab].clone();
                let group_id = match &current_tab {
                    Tab::Group(id) => id.clone(),
                    _ => {
                        self.status = "Switch to a group tab first".to_string();
                        return;
                    }
                };

                let peer_id = match self.find_peer_by_name_or_id(target) {
                    Some(id) => id,
                    None => {
                        self.status = format!("Peer not found: {}", target);
                        return;
                    }
                };

                // Check if already a member
                if let Some(group) = self.groups.get(&group_id) {
                    if group.members.contains(&peer_id) {
                        self.status = format!("{} is already in this group", self.get_peer_display_name(&peer_id));
                        return;
                    }
                }

                // Get group name
                let group_name = self.groups.get(&group_id)
                    .map(|g| g.name.clone())
                    .unwrap_or_else(|| "Unknown".to_string());

                // Send invite via DM (encrypted with pairwise key)
                let invite = GroupInvite {
                    group_id: group_id.clone(),
                    group_name: group_name.clone(),
                };
                let invite_msg = PlainMessage::group_invite_msg(self.own_id.clone(), invite);
                let _ = msg_tx.send(OutgoingMessage::Direct {
                    target_id: peer_id.clone(),
                    message: invite_msg,
                });

                // Add them to our local group membership
                if let Some(group) = self.groups.get_mut(&group_id) {
                    group.members.push(peer_id.clone());
                }

                let peer_name = self.get_peer_display_name(&peer_id);
                let sys_msg = PlainMessage::system(
                    self.own_id.clone(),
                    format!("{} invited to the group", peer_name),
                );
                self.messages.entry(current_tab).or_insert_with(Vec::new).push(sys_msg);
                self.status = format!("Invited {} to {}", peer_name, group_name);
            }
            "leave" => {
                let current_tab = self.tabs[self.active_tab].clone();
                let group_id = match &current_tab {
                    Tab::Group(id) => id.clone(),
                    _ => {
                        self.status = "Switch to a group tab first".to_string();
                        return;
                    }
                };

                // Tell relay to leave room
                let _ = msg_tx.send(OutgoingMessage::LeaveRoom { group_id: group_id.clone() });

                // Notify group members we're leaving
                if let Some(group) = self.groups.get(&group_id) {
                    let leave_msg = PlainMessage::group(
                        self.own_id.clone(),
                        format!("{} has left the group", self.display_name()),
                        group_id.clone(),
                    );
                    // Mark as system message for display
                    let mut sys_leave = leave_msg.clone();
                    sys_leave.system = true;
                    let member_ids: Vec<String> = group.members.clone();
                    let _ = msg_tx.send(OutgoingMessage::Group {
                        group_id: group_id.clone(),
                        member_ids,
                        message: sys_leave,
                    });
                }

                // Remove group and tab
                let group_name = self.groups.get(&group_id)
                    .map(|g| g.name.clone())
                    .unwrap_or_else(|| "Unknown".to_string());
                self.groups.remove(&group_id);
                self.messages.remove(&current_tab);
                if let Some(idx) = self.tabs.iter().position(|t| t == &current_tab) {
                    self.tabs.remove(idx);
                    if self.active_tab >= self.tabs.len() {
                        self.active_tab = self.tabs.len().saturating_sub(1);
                    }
                }

                self.status = format!("Left group: {}", group_name);
            }
            "members" => {
                let current_tab = self.tabs[self.active_tab].clone();
                let group_id = match &current_tab {
                    Tab::Group(id) => id.clone(),
                    _ => {
                        self.status = "Switch to a group tab first".to_string();
                        return;
                    }
                };

                if let Some(group) = self.groups.get(&group_id) {
                    let mut member_names: Vec<String> = group.members.iter()
                        .map(|id| self.get_peer_display_name(id))
                        .collect();
                    member_names.insert(0, format!("{} (you)", self.display_name()));
                    
                    let members_str = member_names.join(", ");
                    let sys_msg = PlainMessage::system(
                        self.own_id.clone(),
                        format!("Members ({}): {}", member_names.len(), members_str),
                    );
                    self.messages.entry(current_tab).or_insert_with(Vec::new).push(sys_msg);
                    self.status = format!("{} members in group", member_names.len());
                } else {
                    self.status = "Group not found".to_string();
                }
            }
            _ => {
                self.status = "Usage: /group create <name> | invite <peer> | leave | members".to_string();
            }
        }
    }

    fn handle_group_invite(&mut self, msg: PlainMessage, invite: GroupInvite, msg_tx: &mut mpsc::UnboundedSender<OutgoingMessage>) {
        let sender_name = self.get_peer_display_name(&msg.sender);
        let group_id = invite.group_id.clone();
        let group_name = invite.group_name.clone();

        // Auto-join: create group locally, join room on relay
        self.groups.insert(group_id.clone(), GroupInfo {
            name: group_name.clone(),
            members: vec![msg.sender.clone()], // The inviter is a member
        });

        // Create group tab
        let group_tab = Tab::Group(group_id.clone());
        if !self.tabs.contains(&group_tab) {
            self.tabs.push(group_tab.clone());
            self.messages.insert(group_tab.clone(), Vec::new());
        }

        // Join room on relay
        let _ = msg_tx.send(OutgoingMessage::JoinRoom { group_id: group_id.clone() });

        // Add system message
        let sys_msg = PlainMessage::system(
            msg.sender.clone(),
            format!("{} invited you to \"{}\"", sender_name, group_name),
        );
        self.messages.entry(group_tab).or_insert_with(Vec::new).push(sys_msg);

        self.status = format!("Joined group: {} (invited by {})", group_name, sender_name);
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
            let peer_name = self.get_peer_display_name(&call.peer_id);
            let duration = chrono::Utc::now() - call.start_time;
            let mins = duration.num_minutes();
            let secs = duration.num_seconds() % 60;
            let mute_icon = if call.muted { "🔇" } else { "🔊" };
            let mute_hint = if call.muted { " [MUTED]" } else { "" };
            header_line2.push(Span::raw(" | "));
            header_line2.push(Span::styled(
                format!("{} {} ({}:{:02}){}", mute_icon, peer_name, mins, secs, mute_hint),
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

    fn render_messages(&self, f: &mut Frame, area: Rect) {
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
                self.get_peer_display_name(peer_id)
            }
            Tab::Group(group_id) => {
                if let Some(group) = self.groups.get(group_id) {
                    format!("#{}", group.name)
                } else {
                    format!("#group-{}", &group_id[..8.min(group_id.len())])
                }
            }
        }
    }

    // Voice call methods

    fn handle_call_command(&mut self, msg_tx: &mut mpsc::UnboundedSender<OutgoingMessage>) {
        // Must be in a DM tab
        let current_tab = &self.tabs[self.active_tab].clone();
        let peer_id = match current_tab {
            Tab::DirectMessage(id) => id.clone(),
            _ => {
                self.status = "Voice calls only work in DM tabs. Use /dm <peer> first.".to_string();
                return;
            }
        };

        if self.active_call.is_some() {
            self.status = "Already in a call. Use /hangup first.".to_string();
            return;
        }

        // Send call request
        let call_req = PlainMessage::call_request(self.own_id.clone());
        let _ = msg_tx.send(OutgoingMessage::Direct {
            target_id: peer_id.clone(),
            message: call_req,
        });

        let peer_name = self.get_peer_display_name(&peer_id);
        self.status = format!("📞 Calling {}...", peer_name);

        // Add system message to DM
        let sys_msg = PlainMessage::system(
            self.own_id.clone(),
            format!("Calling {}...", peer_name),
        );
        self.messages.entry(current_tab.clone()).or_insert_with(Vec::new).push(sys_msg);
    }

    fn handle_accept_call_command(&mut self, msg_tx: &mut mpsc::UnboundedSender<OutgoingMessage>) {
        let peer_id = match self.pending_call_from.take() {
            Some(id) => id,
            None => {
                self.status = "No incoming call to accept.".to_string();
                return;
            }
        };

        if self.active_call.is_some() {
            self.status = "Already in a call. Use /hangup first.".to_string();
            self.pending_call_from = Some(peer_id);
            return;
        }

        // Send acceptance
        let accept_msg = PlainMessage::call_accept(self.own_id.clone(), true);
        let _ = msg_tx.send(OutgoingMessage::Direct {
            target_id: peer_id.clone(),
            message: accept_msg,
        });

        // Start audio pipeline
        self.start_audio_call(peer_id.clone());
    }

    fn handle_reject_call_command(&mut self, msg_tx: &mut mpsc::UnboundedSender<OutgoingMessage>) {
        let peer_id = match self.pending_call_from.take() {
            Some(id) => id,
            None => {
                self.status = "No incoming call to reject.".to_string();
                return;
            }
        };

        let reject_msg = PlainMessage::call_accept(self.own_id.clone(), false);
        let _ = msg_tx.send(OutgoingMessage::Direct {
            target_id: peer_id.clone(),
            message: reject_msg,
        });

        let peer_name = self.get_peer_display_name(&peer_id);
        self.status = format!("Rejected call from {}", peer_name);

        // Add system message to DM tab
        let dm_tab = Tab::DirectMessage(peer_id.clone());
        let sys_msg = PlainMessage::system(
            self.own_id.clone(),
            format!("Rejected call from {}", peer_name),
        );
        self.messages.entry(dm_tab).or_insert_with(Vec::new).push(sys_msg);
    }

    fn handle_hangup_command(&mut self, msg_tx: &mut mpsc::UnboundedSender<OutgoingMessage>) {
        let call = match self.active_call.take() {
            Some(c) => c,
            None => {
                self.status = "Not in a call.".to_string();
                return;
            }
        };

        // Send hangup
        let hangup_msg = PlainMessage::call_hangup(self.own_id.clone());
        let _ = msg_tx.send(OutgoingMessage::Direct {
            target_id: call.peer_id.clone(),
            message: hangup_msg,
        });

        self.stop_audio_call(&call);
    }

    fn handle_incoming_call_request(&mut self, msg: &PlainMessage, _msg_tx: &mut mpsc::UnboundedSender<OutgoingMessage>) {
        let peer_name = self.get_peer_display_name(&msg.sender);

        if self.active_call.is_some() {
            // Already in a call — auto-reject would be nice but let's just notify
            self.status = format!("📞 Missed call from {} (already in a call)", peer_name);
            return;
        }

        self.pending_call_from = Some(msg.sender.clone());
        self.status = format!("📞 Incoming call from {} — /accept-call or /reject-call", peer_name);

        // Ensure DM tab exists
        let dm_tab = Tab::DirectMessage(msg.sender.clone());
        if !self.tabs.contains(&dm_tab) {
            self.tabs.push(dm_tab.clone());
            self.messages.insert(dm_tab.clone(), Vec::new());
        }

        // Add system message
        let sys_msg = PlainMessage::system(
            msg.sender.clone(),
            format!("📞 Incoming call from {} — /accept-call or /reject-call", peer_name),
        );
        self.messages.entry(dm_tab).or_insert_with(Vec::new).push(sys_msg);
    }

    fn handle_call_response(&mut self, msg: &PlainMessage, accept: bool, _msg_tx: &mut mpsc::UnboundedSender<OutgoingMessage>) {
        let peer_name = self.get_peer_display_name(&msg.sender);
        let dm_tab = Tab::DirectMessage(msg.sender.clone());

        if accept {
            // Peer accepted — start audio
            self.start_audio_call(msg.sender.clone());
        } else {
            self.status = format!("{} rejected the call", peer_name);
            let sys_msg = PlainMessage::system(
                msg.sender.clone(),
                format!("{} rejected the call", peer_name),
            );
            self.messages.entry(dm_tab).or_insert_with(Vec::new).push(sys_msg);
        }
    }

    fn handle_remote_hangup(&mut self, msg: &PlainMessage) {
        if let Some(ref call) = self.active_call {
            if call.peer_id == msg.sender {
                let call = self.active_call.take().unwrap();
                self.stop_audio_call(&call);
            }
        }
    }

    fn start_audio_call(&mut self, peer_id: String) {
        let peer_name = self.get_peer_display_name(&peer_id);

        match AudioPipeline::start() {
            Ok(mut pipeline) => {
                self.audio_capture_rx = pipeline.take_capture_rx();
                self.audio_pipeline = Some(pipeline);
                self.active_call = Some(CallState {
                    peer_id: peer_id.clone(),
                    start_time: chrono::Utc::now(),
                    muted: false,
                });
                self.status = format!("🔊 In call with {} | /mute to toggle mic | /hangup to end", peer_name);

                let dm_tab = Tab::DirectMessage(peer_id);
                let sys_msg = PlainMessage::system(
                    self.own_id.clone(),
                    format!("🔊 Voice call started with {}", peer_name),
                );
                self.messages.entry(dm_tab).or_insert_with(Vec::new).push(sys_msg);
            }
            Err(e) => {
                self.status = format!("❌ Failed to start audio: {}", e);
                let dm_tab = Tab::DirectMessage(peer_id);
                let sys_msg = PlainMessage::system(
                    self.own_id.clone(),
                    format!("❌ Failed to start audio: {}", e),
                );
                self.messages.entry(dm_tab).or_insert_with(Vec::new).push(sys_msg);
            }
        }
    }

    fn stop_audio_call(&mut self, call: &CallState) {
        let peer_name = self.get_peer_display_name(&call.peer_id);
        let duration = chrono::Utc::now() - call.start_time;
        let mins = duration.num_minutes();
        let secs = duration.num_seconds() % 60;

        // Stop audio pipeline
        if let Some(ref pipeline) = self.audio_pipeline {
            pipeline.stop();
        }
        self.audio_pipeline = None;
        self.audio_capture_rx = None;

        self.status = format!("Call with {} ended ({}:{:02})", peer_name, mins, secs);

        let dm_tab = Tab::DirectMessage(call.peer_id.clone());
        let sys_msg = PlainMessage::system(
            self.own_id.clone(),
            format!("📵 Call ended with {} ({}:{:02})", peer_name, mins, secs),
        );
        self.messages.entry(dm_tab).or_insert_with(Vec::new).push(sys_msg);
    }

    // File sharing methods
    
    fn handle_share_command(&mut self, filepath: &str, msg_tx: &mut mpsc::UnboundedSender<OutgoingMessage>) {
        
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
        let total_chunks = ((file_data.len() + FILE_CHUNK_SIZE - 1) / FILE_CHUNK_SIZE) as u32;
        
        // Generate random file ID
        let file_id = format!("{:x}", rand::random::<u64>());
        
        let offer = FileOffer {
            file_id: file_id.clone(),
            filename: filename.clone(),
            size: file_data.len() as u64,
            checksum,
            total_chunks,
        };
        
        // Determine if this is a direct message, global, or group
        let current_tab = &self.tabs[self.active_tab];
        let (is_direct, target_peer) = match current_tab {
            Tab::Global => (false, String::new()),
            Tab::DirectMessage(peer_id) => (true, peer_id.clone()),
            Tab::Group(_group_id) => {
                // For groups, send offer as group message (each member gets it)
                // We'll handle this similarly to global but via group fan-out
                (false, String::new())
            }
        };
        
        // Send offer
        let offer_msg = PlainMessage::file_offer(self.own_id.clone(), offer.clone(), is_direct);
        
        match current_tab {
            Tab::Group(group_id) => {
                if let Some(group) = self.groups.get(group_id) {
                    let member_ids = group.members.clone();
                    let mut group_offer = offer_msg.clone();
                    group_offer.group_id = Some(group_id.clone());
                    let _ = msg_tx.send(OutgoingMessage::Group {
                        group_id: group_id.clone(),
                        member_ids,
                        message: group_offer,
                    });
                }
            }
            Tab::DirectMessage(_) => {
                let _ = msg_tx.send(OutgoingMessage::Direct {
                    target_id: target_peer.clone(),
                    message: offer_msg,
                });
            }
            Tab::Global => {
                let _ = msg_tx.send(OutgoingMessage::Global(offer_msg));
            }
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
                Tab::Group(group_id) => {
                    // Send acceptance directly to the file sender (not entire group)
                    let _ = msg_tx.send(OutgoingMessage::Direct {
                        target_id: pending.from_peer.clone(),
                        message: response_msg,
                    });
                    let _ = group_id; // suppress unused warning
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
                Tab::Group(_group_id) => {
                    // Send rejection directly to the file sender
                    let _ = msg_tx.send(OutgoingMessage::Direct {
                        target_id: pending.from_peer.clone(),
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
            let tab = if let Some(ref group_id) = msg.group_id {
                Tab::Group(group_id.clone())
            } else if msg.direct {
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
                let start = i * FILE_CHUNK_SIZE;
                let end = ((i + 1) * FILE_CHUNK_SIZE).min(transfer.file_data.len());
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

fn generate_group_id() -> String {
    use rand::Rng;
    let random_bytes: Vec<u8> = (0..16).map(|_| rand::thread_rng().gen()).collect();
    hex::encode(random_bytes)
}
