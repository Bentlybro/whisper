use std::path::PathBuf;
use tokio::sync::mpsc;

use crate::client::OutgoingMessage;
use crate::protocol::PlainMessage;

use super::types::Tab;
use super::ChatUI;

impl ChatUI {
    /// Ensure a tab exists; create it if missing
    pub(crate) fn ensure_tab(&mut self, tab: &Tab) {
        if !self.tabs.contains(tab) {
            self.tabs.push(tab.clone());
            self.messages.insert(tab.clone(), Vec::new());
        }
    }

    /// Add a system message to a tab, ensuring the tab exists first
    pub(crate) fn add_system_message(&mut self, tab: &Tab, text: String) {
        self.ensure_tab(tab);
        let sys_msg = PlainMessage::system(self.own_id.clone(), text);
        self.messages.entry(tab.clone()).or_insert_with(Vec::new).push(sys_msg);
    }

    /// Get the display name for a group, falling back to a default
    pub(crate) fn group_name(&self, group_id: &str) -> String {
        self.groups.get(group_id)
            .map(|g| g.name.clone())
            .unwrap_or_else(|| "Group".to_string())
    }

    pub(crate) fn open_dm_tab(&mut self, target: &str, msg_tx: Option<&mpsc::UnboundedSender<OutgoingMessage>>) {
        let peer_id = self.find_peer_by_name_or_id(target);

        if let Some(id) = peer_id {
            let dm_tab = Tab::DirectMessage(id.clone());

            if let Some(idx) = self.tabs.iter().position(|t| t == &dm_tab) {
                self.active_tab = idx;
            } else {
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

    pub(crate) fn find_peer_by_name_or_id(&self, target: &str) -> Option<String> {
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

    pub(crate) fn get_peer_display_name(&self, peer_id: &str) -> String {
        if let Some(info) = self.peers.get(peer_id) {
            if let Some(ref nick) = info.nickname {
                return nick.clone();
            }
        }
        peer_id[..12.min(peer_id.len())].to_string()
    }

    pub(crate) fn display_name(&self) -> String {
        self.own_nickname.clone().unwrap_or_else(|| self.own_id[..12].to_string())
    }

    pub(crate) fn next_tab(&mut self) {
        if !self.tabs.is_empty() {
            self.active_tab = (self.active_tab + 1) % self.tabs.len();
        }
    }

    pub(crate) fn prev_tab(&mut self) {
        if !self.tabs.is_empty() {
            self.active_tab = if self.active_tab == 0 {
                self.tabs.len() - 1
            } else {
                self.active_tab - 1
            };
        }
    }

    pub(crate) fn get_tab_name(&self, tab: &Tab) -> String {
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
}

/// Generate a random group ID
pub fn generate_group_id() -> String {
    use rand::Rng;
    let random_bytes: Vec<u8> = (0..16).map(|_| rand::thread_rng().gen()).collect();
    hex::encode(random_bytes)
}

/// Format a duration smartly: "1:23" for under an hour, "2:45:03" for hours, "1d 3:20:15" for days
pub fn format_duration(duration: chrono::Duration) -> String {
    let total_secs = duration.num_seconds();
    let days = total_secs / 86400;
    let hours = (total_secs % 86400) / 3600;
    let mins = (total_secs % 3600) / 60;
    let secs = total_secs % 60;

    if days > 0 {
        format!("{}d {}:{:02}:{:02}", days, hours, mins, secs)
    } else if hours > 0 {
        format!("{}:{:02}:{:02}", hours, mins, secs)
    } else {
        format!("{}:{:02}", mins, secs)
    }
}

/// Expand ~ in paths to the user's home directory
pub fn expand_path(path: &str) -> PathBuf {
    if let Some(stripped) = path.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
            let mut buf = PathBuf::from(home);
            buf.push(stripped);
            return buf;
        }
    }
    PathBuf::from(path)
}
