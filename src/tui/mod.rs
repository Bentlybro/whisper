mod calls;
mod commands;
mod files;
mod groups;
mod helpers;
mod render;
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
    ActiveTransfer, CallState, CallType, GroupInfo, OutgoingTransfer, PendingFileOffer, Tab,
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
        }
    }

    pub async fn run(
        &mut self,
        mut msg_tx: mpsc::UnboundedSender<OutgoingMessage>,
        mut incoming_rx: mpsc::UnboundedReceiver<PlainMessage>,
        mut status_rx: mpsc::UnboundedReceiver<String>,
        mut peer_update_rx: mpsc::UnboundedReceiver<HashMap<String, PeerDisplay>>,
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
        peer_update_rx: &mut mpsc::UnboundedReceiver<HashMap<String, PeerDisplay>>,
        audio_in_rx: &mut mpsc::UnboundedReceiver<(String, Vec<u8>)>,
    ) -> Result<()> {
        let mut opus_decoder: Option<audiopus::coder::Decoder> = None;
        loop {
            terminal.draw(|f| self.ui(f))?;

            // Handle events with timeout
            if event::poll(std::time::Duration::from_millis(100))? {
                if let Event::Key(key) = event::read()? {
                    if key.kind == KeyEventKind::Press {
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
                    self.handle_remote_hangup(&msg, msg_tx);
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
