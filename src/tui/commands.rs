use tokio::sync::mpsc;

use crate::client::OutgoingMessage;
use crate::crypto::safety_number::compute_safety_number;
use crate::protocol::PlainMessage;

use super::types::Tab;
use super::ChatUI;

impl ChatUI {
    pub(crate) fn handle_input(&mut self, text: String, msg_tx: &mut mpsc::UnboundedSender<OutgoingMessage>) {
        // Handle commands
        let trimmed = text.trim();
        if trimmed.starts_with('/') {
            let parts: Vec<&str> = trimmed[1..].split_whitespace().collect();
            if parts.is_empty() {
                self.status = "Empty command".to_string();
                return;
            }

            match parts[0] {
                "help" => {
                    // Show help as a system message in current tab
                    let tab = self.tabs[self.active_tab].clone();
                    let commands = Self::get_all_commands();
                    let mut help_text = String::from("Available commands:\n");
                    for cmd in &commands {
                        help_text.push_str(&format!("  /{:<16} {}\n", cmd.name, cmd.description));
                    }
                    help_text.push_str("\nTip: Type / to see interactive autocomplete!");
                    let msg = PlainMessage::system("system".to_string(), help_text);
                    self.messages.entry(tab).or_insert_with(Vec::new).push(msg);
                    self.status = "Showing help".to_string();
                    return;
                }
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
                "verify" => {
                    self.handle_verify_command(&parts[1..], msg_tx);
                    return;
                }
                "verified" => {
                    // Mark current verify target as verified
                    self.handle_mark_verified(&parts[1..]);
                    return;
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
                "share-screen" => {
                    self.handle_share_screen_command(msg_tx);
                }
                "stop-share" => {
                    self.handle_stop_share_command(msg_tx);
                }
                "accept-screen" => {
                    self.handle_accept_screen_command(msg_tx);
                }
                "reject-screen" => {
                    self.handle_reject_screen_command(msg_tx);
                }
                _ => {
                    self.status = format!("Unknown command: /{}", parts[0]);
                }
            }
            return;
        }

        // Regular message (falls through from command handling above)
        let current_tab = &self.tabs[self.active_tab].clone();

        // Reset scroll to bottom when sending a message
        self.scroll_offset.insert(current_tab.clone(), 0);

        match current_tab {
            Tab::Global => {
                let mut msg = PlainMessage::new(self.own_id.clone(), text);
                let msg_id = PlainMessage::generate_id();
                msg.message_id = Some(msg_id.clone());
                self.read_status.insert(msg_id, super::types::ReadStatus::Sent);
                self.messages.entry(Tab::Global).or_insert_with(Vec::new).push(msg.clone());
                let _ = msg_tx.send(OutgoingMessage::Global(msg));
            }
            Tab::DirectMessage(peer_id) => {
                let mut msg = PlainMessage::direct(self.own_id.clone(), text);
                let msg_id = PlainMessage::generate_id();
                msg.message_id = Some(msg_id.clone());
                self.read_status.insert(msg_id, super::types::ReadStatus::Sent);
                self.messages.entry(current_tab.clone()).or_insert_with(Vec::new).push(msg.clone());
                let _ = msg_tx.send(OutgoingMessage::Direct {
                    target_id: peer_id.clone(),
                    message: msg,
                });
            }
            Tab::Group(group_id) => {
                if let Some(group) = self.groups.get(group_id) {
                    let mut msg = PlainMessage::group(self.own_id.clone(), text, group_id.clone());
                    let msg_id = PlainMessage::generate_id();
                    msg.message_id = Some(msg_id.clone());
                    self.read_status.insert(msg_id, super::types::ReadStatus::Sent);
                    self.messages.entry(current_tab.clone()).or_insert_with(Vec::new).push(msg.clone());
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

    /// Handle /verify [nickname|peer_id] — show safety number for a peer
    /// If no argument, try to use the current DM tab's peer
    fn handle_verify_command(&mut self, args: &[&str], _msg_tx: &mut mpsc::UnboundedSender<OutgoingMessage>) {
        let peer_id = if args.is_empty() {
            // Try current tab
            match &self.tabs[self.active_tab] {
                Tab::DirectMessage(id) => Some(id.clone()),
                _ => None,
            }
        } else {
            self.find_peer_by_name_or_id(args[0])
        };

        let peer_id = match peer_id {
            Some(id) => id,
            None => {
                if args.is_empty() {
                    self.status = "Usage: /verify <nickname|peer_id> (or use in a DM tab)".to_string();
                } else {
                    self.status = format!("Peer not found: {}", args[0]);
                }
                return;
            }
        };

        let peer = match self.peers.get(&peer_id) {
            Some(p) => p,
            None => {
                self.status = "Peer not connected".to_string();
                return;
            }
        };

        if peer.public_key.is_empty() {
            self.status = "No public key available for this peer yet".to_string();
            return;
        }

        let safety_number = compute_safety_number(&self.own_public_key, &peer.public_key);
        let peer_name = self.get_peer_display_name(&peer_id);
        let verified = if self.verified_peers.contains(&peer_id) { " ✅" } else { "" };

        // Show as a system message in the current tab
        let tab = self.tabs[self.active_tab].clone();
        let msg = PlainMessage::system(
            "system".to_string(),
            format!(
                "🔐 Safety Number with {}{}\n  Numbers: {}\n  Emoji:   {}\n\nBoth sides should see the same code.\nIf they match, run /verified {} to mark as verified.",
                peer_name,
                verified,
                safety_number.numeric(),
                safety_number.emoji(),
                args.first().copied().unwrap_or(&peer_id[..12.min(peer_id.len())]),
            ),
        );
        self.messages.entry(tab).or_insert_with(Vec::new).push(msg);
        self.status = format!("Safety number shown for {}", peer_name);
    }

    /// Handle /verified <nickname|peer_id> — mark a peer as verified
    fn handle_mark_verified(&mut self, args: &[&str]) {
        let peer_id = if args.is_empty() {
            match &self.tabs[self.active_tab] {
                Tab::DirectMessage(id) => Some(id.clone()),
                _ => None,
            }
        } else {
            self.find_peer_by_name_or_id(args[0])
        };

        let peer_id = match peer_id {
            Some(id) => id,
            None => {
                if args.is_empty() {
                    self.status = "Usage: /verified <nickname|peer_id> (or use in a DM tab)".to_string();
                } else {
                    self.status = format!("Peer not found: {}", args[0]);
                }
                return;
            }
        };

        let peer_name = self.get_peer_display_name(&peer_id);
        self.verified_peers.insert(peer_id);
        self.status = format!("✅ {} marked as verified", peer_name);

        // Show confirmation in current tab
        let tab = self.tabs[self.active_tab].clone();
        let msg = PlainMessage::system(
            "system".to_string(),
            format!("✅ {} is now verified — identity confirmed!", peer_name),
        );
        self.messages.entry(tab).or_insert_with(Vec::new).push(msg);
    }
}
