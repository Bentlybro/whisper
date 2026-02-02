mod calls;
mod commands;
mod files;
mod groups;
mod helpers;
mod render;
mod screen_share;
mod types;

use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::collections::HashMap;
use std::io;
use tokio::sync::mpsc;

use crate::audio::AudioPipeline;
use crate::client::{OutgoingMessage, PeerDisplay};
use crate::protocol::PlainMessage;

use types::{
    ActiveTransfer, AutocompleteState, CallState, CallType, CommandEntry, GroupInfo,
    OutgoingTransfer, PendingFileOffer, ReadStatus, Tab,
};

pub struct ChatUI {
    pub(crate) tabs: Vec<Tab>,
    pub(crate) active_tab: usize,
    pub(crate) messages: HashMap<Tab, Vec<PlainMessage>>,
    pub(crate) input: Vec<char>,
    pub(crate) cursor: usize,
    pub(crate) status: String,
    pub(crate) peers: HashMap<String, PeerDisplay>,
    pub(crate) own_id: String,
    pub(crate) own_nickname: Option<String>,
    /// Our own identity public key (for safety number computation)
    pub(crate) own_public_key: Vec<u8>,
    /// Peers we've manually verified via /verify
    pub(crate) verified_peers: std::collections::HashSet<String>,
    pub(crate) pending_offers: HashMap<String, PendingFileOffer>,
    pub(crate) active_transfers: HashMap<String, ActiveTransfer>,
    pub(crate) outgoing_transfers: HashMap<String, OutgoingTransfer>,
    pub(crate) groups: HashMap<String, GroupInfo>,
    // Voice call state
    pub(crate) active_call: Option<CallState>,
    pub(crate) pending_call_from: Option<String>,
    pub(crate) pending_group_call: Option<(String, String)>,
    pub(crate) audio_pipeline: Option<AudioPipeline>,
    pub(crate) audio_capture_rx: Option<mpsc::UnboundedReceiver<Vec<u8>>>,
    // Scroll state per tab (0 = at bottom)
    pub(crate) scroll_offset: HashMap<Tab, usize>,
    // Typing indicators: peer_id -> last typing timestamp
    pub(crate) typing_peers: HashMap<String, std::time::Instant>,
    pub(crate) last_typing_sent: Option<std::time::Instant>,
    // Read receipts: message_id -> ReadStatus
    pub(crate) read_status: HashMap<String, ReadStatus>,
    // Command autocomplete state
    pub(crate) autocomplete: Option<AutocompleteState>,
    // Screen sharing state
    pub(crate) screen_capture: Option<crate::screen::capture::ScreenCapture>,
    pub(crate) screen_capture_rx: Option<mpsc::UnboundedReceiver<crate::screen::ScreenFrameData>>,
    pub(crate) screen_share_target: Option<String>, // peer_id we're sharing to
    pub(crate) screen_viewer_from: Option<String>, // peer_id sharing with us
    pub(crate) pending_screen_share_from: Option<String>, // incoming screen share request
    /// Current frame to render in TUI (shared by both sharer preview & viewer)
    pub(crate) screen_frame: Option<crate::screen::viewer::DecodedFrame>,
    /// Whether we're in screen share view mode (full-screen frame display)
    pub(crate) screen_view_active: bool,
}

impl ChatUI {
    pub fn new(own_id: String, nickname: Option<String>, own_public_key: Vec<u8>) -> Self {
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
            own_public_key,
            verified_peers: std::collections::HashSet::new(),
            pending_offers: HashMap::new(),
            active_transfers: HashMap::new(),
            outgoing_transfers: HashMap::new(),
            groups: HashMap::new(),
            active_call: None,
            pending_call_from: None,
            pending_group_call: None,
            audio_pipeline: None,
            audio_capture_rx: None,
            scroll_offset: HashMap::new(),
            typing_peers: HashMap::new(),
            last_typing_sent: None,
            read_status: HashMap::new(),
            autocomplete: None,
            screen_capture: None,
            screen_capture_rx: None,
            screen_share_target: None,
            screen_viewer_from: None,
            pending_screen_share_from: None,
            screen_frame: None,
            screen_view_active: false,
        }
    }

    pub async fn run(
        &mut self,
        mut msg_tx: mpsc::UnboundedSender<OutgoingMessage>,
        mut incoming_rx: mpsc::UnboundedReceiver<PlainMessage>,
        mut status_rx: mpsc::UnboundedReceiver<String>,
        mut peer_update_rx: mpsc::UnboundedReceiver<HashMap<String, PeerDisplay>>,
        mut audio_in_rx: mpsc::UnboundedReceiver<(String, Vec<u8>)>,
        mut screen_in_rx: mpsc::UnboundedReceiver<(String, Vec<u8>)>,
    ) -> Result<()> {
        // Setup terminal - no mouse capture so native text selection works
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen)?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;

        let result = self.run_loop(&mut terminal, &mut msg_tx, &mut incoming_rx, &mut status_rx, &mut peer_update_rx, &mut audio_in_rx, &mut screen_in_rx).await;

        // Restore terminal
        disable_raw_mode()?;
        execute!(
            terminal.backend_mut(),
            LeaveAlternateScreen,
        )?;
        terminal.show_cursor()?;

        result
    }

    /// Get all available commands for autocomplete
    pub(crate) fn get_all_commands() -> Vec<CommandEntry> {
        vec![
            CommandEntry { name: "help".to_string(), description: "Show this command list".to_string() },
            CommandEntry { name: "dm".to_string(), description: "Open DM with a peer: /dm <nick|id>".to_string() },
            CommandEntry { name: "nick".to_string(), description: "Change nickname: /nick <name>".to_string() },
            CommandEntry { name: "group".to_string(), description: "Group commands: create/invite/leave/members".to_string() },
            CommandEntry { name: "call".to_string(), description: "Start a voice call in current tab".to_string() },
            CommandEntry { name: "accept-call".to_string(), description: "Accept incoming call".to_string() },
            CommandEntry { name: "reject-call".to_string(), description: "Reject incoming call".to_string() },
            CommandEntry { name: "hangup".to_string(), description: "End current call".to_string() },
            CommandEntry { name: "mute".to_string(), description: "Toggle microphone mute".to_string() },
            CommandEntry { name: "verify".to_string(), description: "Show safety number for peer".to_string() },
            CommandEntry { name: "verified".to_string(), description: "Mark peer as verified".to_string() },
            CommandEntry { name: "send".to_string(), description: "Share a file: /send <filepath>".to_string() },
            CommandEntry { name: "accept".to_string(), description: "Accept file offer: /accept [path]".to_string() },
            CommandEntry { name: "reject".to_string(), description: "Reject file offer".to_string() },
            CommandEntry { name: "share-screen".to_string(), description: "Start sharing your screen".to_string() },
            CommandEntry { name: "stop-share".to_string(), description: "Stop screen sharing".to_string() },
            CommandEntry { name: "accept-screen".to_string(), description: "Accept incoming screen share".to_string() },
            CommandEntry { name: "reject-screen".to_string(), description: "Reject incoming screen share".to_string() },
        ]
    }

    /// Update autocomplete state based on current input
    fn update_autocomplete(&mut self) {
        let input_str: String = self.input.iter().collect();
        if input_str.starts_with('/') && !input_str.contains(' ') {
            let filter = input_str[1..].to_lowercase();
            let commands = Self::get_all_commands();
            let filtered: Vec<usize> = commands.iter().enumerate()
                .filter(|(_, cmd)| cmd.name.starts_with(&filter))
                .map(|(i, _)| i)
                .collect();

            if !filtered.is_empty() {
                let selected = if let Some(ref ac) = self.autocomplete {
                    ac.selected.min(filtered.len().saturating_sub(1))
                } else {
                    0
                };
                self.autocomplete = Some(AutocompleteState {
                    commands,
                    filtered,
                    selected,
                    filter,
                });
            } else {
                self.autocomplete = None;
            }
        } else {
            self.autocomplete = None;
        }
    }

        /// Send typing indicator to current tab's peers (debounced, bypasses ratchet)
    fn send_typing_indicator(&mut self, msg_tx: &mut mpsc::UnboundedSender<OutgoingMessage>) {
        use crate::protocol::Message;
        
        let now = std::time::Instant::now();
        // Debounce: only send every 3 seconds
        if let Some(last) = self.last_typing_sent {
            if now.duration_since(last).as_secs() < 3 {
                return;
            }
        }
        self.last_typing_sent = Some(now);

        let current_tab = &self.tabs[self.active_tab].clone();
        match current_tab {
            Tab::DirectMessage(peer_id) => {
                let _ = msg_tx.send(OutgoingMessage::Signal(Message::Typing {
                    from: self.own_id.clone(),
                    target: peer_id.clone(),
                    is_typing: true,
                }));
            }
            Tab::Group(group_id) => {
                if let Some(group) = self.groups.get(group_id) {
                    for member_id in &group.members {
                        let _ = msg_tx.send(OutgoingMessage::Signal(Message::Typing {
                            from: self.own_id.clone(),
                            target: member_id.clone(),
                            is_typing: true,
                        }));
                    }
                }
            }
            Tab::Global => {
                // Send to all peers
                for peer_id in self.peers.keys() {
                    let _ = msg_tx.send(OutgoingMessage::Signal(Message::Typing {
                        from: self.own_id.clone(),
                        target: peer_id.clone(),
                        is_typing: true,
                    }));
                }
            }
        }
    }

    /// Send read receipts for visible messages in the current tab (bypasses ratchet)
    fn send_read_receipts(&mut self, msg_tx: &mut mpsc::UnboundedSender<OutgoingMessage>) {
        use crate::protocol::Message;
        
        let current_tab = self.tabs[self.active_tab].clone();
        let messages = match self.messages.get(&current_tab) {
            Some(msgs) => msgs.clone(),
            None => return,
        };

        for msg in &messages {
            if msg.sender == self.own_id || msg.system {
                continue;
            }
            if let Some(ref msg_id) = msg.message_id {
                if self.read_status.get(msg_id) == Some(&ReadStatus::Read) {
                    continue; // Already sent read receipt
                }
                // Mark as read and send receipt via signal (no ratchet)
                self.read_status.insert(msg_id.clone(), ReadStatus::Read);

                let _ = msg_tx.send(OutgoingMessage::Signal(Message::ReadReceipt {
                    from: self.own_id.clone(),
                    target: msg.sender.clone(),
                    message_id: msg_id.clone(),
                }));
            }
        }
    }

    /// Clean up expired typing indicators (>5 seconds old)
    fn cleanup_typing_indicators(&mut self) {
        let now = std::time::Instant::now();
        self.typing_peers.retain(|_, instant| {
            now.duration_since(*instant).as_secs() < 5
        });
    }

    async fn run_loop(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
        msg_tx: &mut mpsc::UnboundedSender<OutgoingMessage>,
        incoming_rx: &mut mpsc::UnboundedReceiver<PlainMessage>,
        status_rx: &mut mpsc::UnboundedReceiver<String>,
        peer_update_rx: &mut mpsc::UnboundedReceiver<HashMap<String, PeerDisplay>>,
        audio_in_rx: &mut mpsc::UnboundedReceiver<(String, Vec<u8>)>,
        screen_in_rx: &mut mpsc::UnboundedReceiver<(String, Vec<u8>)>,
    ) -> Result<()> {
        let mut opus_decoder: Option<audiopus::coder::Decoder> = None;
        let mut read_receipt_timer = std::time::Instant::now();
        loop {
            terminal.draw(|f| self.ui(f))?;

            // Periodically clean up typing indicators and send read receipts
            self.cleanup_typing_indicators();
            if read_receipt_timer.elapsed().as_secs() >= 2 {
                self.send_read_receipts(msg_tx);
                read_receipt_timer = std::time::Instant::now();
            }

            // Handle events with timeout
            if event::poll(std::time::Duration::from_millis(100))? {
                if let Event::Key(key) = event::read()? {
                    if key.kind == KeyEventKind::Press {
                        // Toggle screen share view with Escape
                        if self.screen_view_active && key.code == KeyCode::Esc {
                            self.screen_view_active = false;
                            continue;
                        }
                        // Re-enter screen share view with F5 (when a share session exists)
                        if key.code == KeyCode::F(5) && (self.screen_share_target.is_some() || self.screen_viewer_from.is_some()) {
                            self.screen_view_active = !self.screen_view_active;
                            continue;
                        }

                        // Handle autocomplete navigation first
                        if self.autocomplete.is_some() {
                            match key.code {
                                KeyCode::Up => {
                                    if let Some(ref mut ac) = self.autocomplete {
                                        if ac.selected > 0 {
                                            ac.selected -= 1;
                                        } else {
                                            ac.selected = ac.filtered.len().saturating_sub(1);
                                        }
                                    }
                                    continue;
                                }
                                KeyCode::Down => {
                                    if let Some(ref mut ac) = self.autocomplete {
                                        if ac.selected < ac.filtered.len().saturating_sub(1) {
                                            ac.selected += 1;
                                        } else {
                                            ac.selected = 0;
                                        }
                                    }
                                    continue;
                                }
                                KeyCode::Enter => {
                                    if let Some(ref ac) = self.autocomplete {
                                        if let Some(&cmd_idx) = ac.filtered.get(ac.selected) {
                                            let cmd_name = ac.commands[cmd_idx].name.clone();
                                            self.input = format!("/{} ", cmd_name).chars().collect();
                                            self.cursor = self.input.len();
                                        }
                                    }
                                    self.autocomplete = None;
                                    continue;
                                }
                                KeyCode::Esc => {
                                    self.autocomplete = None;
                                    continue;
                                }
                                KeyCode::Tab => {
                                    // Tab-complete the selected command
                                    if let Some(ref ac) = self.autocomplete {
                                        if let Some(&cmd_idx) = ac.filtered.get(ac.selected) {
                                            let cmd_name = ac.commands[cmd_idx].name.clone();
                                            self.input = format!("/{} ", cmd_name).chars().collect();
                                            self.cursor = self.input.len();
                                        }
                                    }
                                    self.autocomplete = None;
                                    continue;
                                }
                                _ => {
                                    // Fall through to normal handling, autocomplete will update
                                }
                            }
                        }

                        match key.code {
                            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
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
                            // Scroll: Up/Down with Alt, PgUp/PgDown
                            KeyCode::Up if key.modifiers.contains(KeyModifiers::ALT) => {
                                self.scroll_up(1);
                            }
                            KeyCode::Down if key.modifiers.contains(KeyModifiers::ALT) => {
                                self.scroll_down(1);
                            }
                            KeyCode::PageUp => {
                                self.scroll_up(10);
                            }
                            KeyCode::PageDown => {
                                self.scroll_down(10);
                            }
                            KeyCode::Char(c) => {
                                self.input.insert(self.cursor, c);
                                self.cursor += 1;
                                self.update_autocomplete();
                                // Send typing indicator for non-command input
                                if !self.input.starts_with(&['/']) {
                                    self.send_typing_indicator(msg_tx);
                                }
                            }
                            KeyCode::Backspace => {
                                if self.cursor > 0 {
                                    self.cursor -= 1;
                                    self.input.remove(self.cursor);
                                    self.update_autocomplete();
                                }
                            }
                            KeyCode::Delete => {
                                if self.cursor < self.input.len() {
                                    self.input.remove(self.cursor);
                                    self.update_autocomplete();
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
                                self.input.insert(self.cursor, '\n');
                                self.cursor += 1;
                            }
                            KeyCode::Enter => {
                                if !self.input.is_empty() {
                                    let text: String = self.input.iter().collect();
                                    self.handle_input(text, msg_tx);
                                    self.input.clear();
                                    self.cursor = 0;
                                    self.autocomplete = None;
                                    self.last_typing_sent = None;
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }

            // Check for incoming messages
            while let Ok(msg) = incoming_rx.try_recv() {
                // Handle typing indicators
                if let Some(is_typing) = msg.typing {
                    if is_typing {
                        self.typing_peers.insert(msg.sender.clone(), std::time::Instant::now());
                    } else {
                        self.typing_peers.remove(&msg.sender);
                    }
                    continue;
                }

                // Handle read receipts
                if let Some(ref receipt_msg_id) = msg.read_receipt {
                    self.read_status.insert(receipt_msg_id.clone(), ReadStatus::Read);
                    continue;
                }

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
                    self.handle_remote_hangup(&msg, msg_tx);
                    continue;
                }

                // Handle screen share signaling
                if msg.screen_share_request == Some(true) {
                    self.handle_incoming_screen_request(&msg);
                    continue;
                }
                if let Some(accept) = msg.screen_share_accept {
                    self.handle_screen_share_response(&msg, accept);
                    continue;
                }
                if msg.screen_share_stop == Some(true) {
                    self.handle_screen_share_stop(&msg);
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

                // Clear typing indicator for sender (they sent a real message)
                self.typing_peers.remove(&msg.sender);

                // Track read status for own messages
                if msg.sender == self.own_id {
                    if let Some(ref msg_id) = msg.message_id {
                        self.read_status.entry(msg_id.clone()).or_insert(ReadStatus::Sent);
                    }
                }

                // Auto-scroll to bottom on new messages if at bottom
                let target_tab = if let Some(ref group_id) = msg.group_id {
                    Tab::Group(group_id.clone())
                } else if msg.direct {
                    Tab::DirectMessage(msg.sender.clone())
                } else {
                    Tab::Global
                };
                let scroll = self.scroll_offset.get(&target_tab).copied().unwrap_or(0);
                if scroll == 0 {
                    // Already at bottom, stay there (default behavior)
                }

                // Handle group messages
                if let Some(ref group_id) = msg.group_id {
                    let group_tab = Tab::Group(group_id.clone());
                    self.ensure_tab(&group_tab);
                    self.messages.entry(group_tab).or_insert_with(Vec::new).push(msg);
                    continue;
                }

                if msg.dm_request {
                    let sender_id = msg.sender.clone();
                    let dm_tab = Tab::DirectMessage(sender_id.clone());
                    if !self.tabs.contains(&dm_tab) {
                        self.tabs.push(dm_tab.clone());
                        self.messages.insert(dm_tab, Vec::new());
                        let peer_name = self.get_peer_display_name(&sender_id);
                        self.status = format!("{} opened a DM with you", peer_name);
                    }
                } else if msg.system && !msg.content.is_empty() {
                    self.messages.entry(Tab::Global).or_insert_with(Vec::new).push(msg);
                } else if !msg.system {
                    let sender_id = msg.sender.clone();

                    if msg.direct {
                        let dm_tab = Tab::DirectMessage(sender_id.clone());
                        self.ensure_tab(&dm_tab);
                        self.messages.entry(dm_tab).or_insert_with(Vec::new).push(msg);
                    } else {
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
                    let accept = match &call.call_type {
                        CallType::Direct(peer_id) => *peer_id == from,
                        CallType::Group { group_id } => {
                            self.groups.get(group_id)
                                .map(|g| g.members.contains(&from))
                                .unwrap_or(false)
                        }
                    };
                    if accept {
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

            // Handle incoming screen frames — decode and store for TUI rendering
            while let Ok((from, frame_data)) = screen_in_rx.try_recv() {
                // Only accept frames from the peer we're viewing
                let accept = self.screen_viewer_from.as_ref() == Some(&from);
                if accept {
                    if let Ok(frame) = rmp_serde::from_slice::<crate::screen::ScreenFrameData>(&frame_data) {
                        if let Ok(decoded) = crate::screen::viewer::DecodedFrame::from_frame(&frame) {
                            let is_first_frame = self.screen_frame.is_none();
                            self.screen_frame = Some(decoded);
                            // Only auto-enter on the FIRST frame, not every frame
                            if is_first_frame {
                                self.screen_view_active = true;
                            }
                        }
                    }
                }
            }

            // Send captured screen frames to peer + show local preview
            if let Some(ref target_id) = self.screen_share_target.clone() {
                if let Some(ref mut capture_rx) = self.screen_capture_rx {
                    // Drain all, keep latest (drop stale frames)
                    let mut latest = None;
                    while let Ok(frame) = capture_rx.try_recv() {
                        latest = Some(frame);
                    }
                    if let Some(frame) = latest {
                        // Decode for local preview
                        if let Ok(decoded) = crate::screen::viewer::DecodedFrame::from_frame(&frame) {
                            let is_first_frame = self.screen_frame.is_none();
                            self.screen_frame = Some(decoded);
                            // Only auto-enter on first frame
                            if is_first_frame {
                                self.screen_view_active = true;
                            }
                        }
                        // Send to peer
                        if let Ok(serialized) = rmp_serde::to_vec(&frame) {
                            let _ = msg_tx.send(OutgoingMessage::ScreenFrame {
                                target_id: target_id.clone(),
                                data: serialized,
                            });
                        }
                    }
                }

                if !self.screen_capture.is_some() {
                    self.screen_share_target = None;
                }
            }

            // Send captured audio frames to peer(s) (unless muted)
            if let Some(ref call) = self.active_call {
                let is_muted = call.muted;
                let target_ids: Vec<String> = match &call.call_type {
                    CallType::Direct(peer_id) => vec![peer_id.clone()],
                    CallType::Group { group_id } => {
                        self.groups.get(group_id)
                            .map(|g| g.members.clone())
                            .unwrap_or_default()
                    }
                };
                if let Some(ref mut capture_rx) = self.audio_capture_rx {
                    while let Ok(opus_frame) = capture_rx.try_recv() {
                        if !is_muted {
                            for target_id in &target_ids {
                                let _ = msg_tx.send(OutgoingMessage::Audio {
                                    target_id: target_id.clone(),
                                    data: opus_frame.clone(),
                                });
                            }
                        }
                    }
                }
            }
        }
    }
}
