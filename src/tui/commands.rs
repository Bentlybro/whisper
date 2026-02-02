use tokio::sync::mpsc;

use crate::client::OutgoingMessage;
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
                self.messages.entry(Tab::Global).or_insert_with(Vec::new).push(msg.clone());
                let _ = msg_tx.send(OutgoingMessage::Global(msg));
            }
            Tab::DirectMessage(peer_id) => {
                let msg = PlainMessage::direct(self.own_id.clone(), text);
                self.messages.entry(current_tab.clone()).or_insert_with(Vec::new).push(msg.clone());
                let _ = msg_tx.send(OutgoingMessage::Direct {
                    target_id: peer_id.clone(),
                    message: msg,
                });
            }
            Tab::Group(group_id) => {
                if let Some(group) = self.groups.get(group_id) {
                    let msg = PlainMessage::group(self.own_id.clone(), text, group_id.clone());
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
}
