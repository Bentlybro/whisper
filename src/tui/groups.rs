use tokio::sync::mpsc;

use crate::client::OutgoingMessage;
use crate::protocol::{GroupInvite, PlainMessage};

use super::helpers::generate_group_id;
use super::types::{GroupInfo, Tab};
use super::ChatUI;

impl ChatUI {
    pub(crate) fn handle_group_command(&mut self, parts: &[&str], msg_tx: &mut mpsc::UnboundedSender<OutgoingMessage>) {
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

                self.groups.insert(group_id.clone(), GroupInfo {
                    name: group_name.clone(),
                    members: Vec::new(),
                });

                let group_tab = Tab::Group(group_id.clone());
                self.tabs.push(group_tab.clone());
                self.messages.insert(group_tab.clone(), Vec::new());
                self.active_tab = self.tabs.len() - 1;

                let _ = msg_tx.send(OutgoingMessage::JoinRoom { group_id: group_id.clone() });

                self.add_system_message(
                    &group_tab,
                    format!("Group \"{}\" created. Use /group invite <peer> to add members.", group_name),
                );

                self.status = format!("Created group: {} ({})", group_name, &group_id[..8]);
            }
            "invite" => {
                if parts.len() < 2 {
                    self.status = "Usage: /group invite <nickname|peer_id>".to_string();
                    return;
                }
                let target = parts[1];

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

                if let Some(group) = self.groups.get(&group_id) {
                    if group.members.contains(&peer_id) {
                        self.status = format!("{} is already in this group", self.get_peer_display_name(&peer_id));
                        return;
                    }
                }

                let group_name = self.group_name(&group_id);

                let invite = GroupInvite {
                    group_id: group_id.clone(),
                    group_name: group_name.clone(),
                };
                let invite_msg = PlainMessage::group_invite_msg(self.own_id.clone(), invite);
                let _ = msg_tx.send(OutgoingMessage::Direct {
                    target_id: peer_id.clone(),
                    message: invite_msg,
                });

                if let Some(group) = self.groups.get_mut(&group_id) {
                    group.members.push(peer_id.clone());
                }

                let peer_name = self.get_peer_display_name(&peer_id);
                self.add_system_message(&current_tab, format!("{} invited to the group", peer_name));
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

                let _ = msg_tx.send(OutgoingMessage::LeaveRoom { group_id: group_id.clone() });

                if let Some(group) = self.groups.get(&group_id) {
                    let leave_msg = PlainMessage::group(
                        self.own_id.clone(),
                        format!("{} has left the group", self.display_name()),
                        group_id.clone(),
                    );
                    let mut sys_leave = leave_msg.clone();
                    sys_leave.system = true;
                    let member_ids: Vec<String> = group.members.clone();
                    let _ = msg_tx.send(OutgoingMessage::Group {
                        group_id: group_id.clone(),
                        member_ids,
                        message: sys_leave,
                    });
                }

                let group_name = self.group_name(&group_id);
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
                    self.add_system_message(
                        &current_tab,
                        format!("Members ({}): {}", member_names.len(), members_str),
                    );
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

    pub(crate) fn handle_group_invite(&mut self, msg: PlainMessage, invite: GroupInvite, msg_tx: &mut mpsc::UnboundedSender<OutgoingMessage>) {
        let sender_name = self.get_peer_display_name(&msg.sender);
        let group_id = invite.group_id.clone();
        let group_name = invite.group_name.clone();

        self.groups.insert(group_id.clone(), GroupInfo {
            name: group_name.clone(),
            members: vec![msg.sender.clone()],
        });

        let group_tab = Tab::Group(group_id.clone());
        self.ensure_tab(&group_tab);

        let _ = msg_tx.send(OutgoingMessage::JoinRoom { group_id: group_id.clone() });

        // Use sender's ID for system message attribution (not our own)
        let sys_msg = PlainMessage::system(
            msg.sender.clone(),
            format!("{} invited you to \"{}\"", sender_name, group_name),
        );
        self.messages.entry(group_tab).or_insert_with(Vec::new).push(sys_msg);

        self.status = format!("Joined group: {} (invited by {})", group_name, sender_name);
    }
}
