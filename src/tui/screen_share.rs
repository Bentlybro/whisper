use tokio::sync::mpsc;

use crate::client::OutgoingMessage;
use crate::protocol::PlainMessage;
use crate::screen::capture::ScreenCapture;

use super::types::Tab;
use super::ChatUI;

impl ChatUI {
    pub(crate) fn handle_share_screen_command(
        &mut self,
        msg_tx: &mut mpsc::UnboundedSender<OutgoingMessage>,
    ) {
        if self.screen_share_target.is_some() {
            self.status = "Already sharing screen. Use /stop-share first.".to_string();
            return;
        }

        let current_tab = self.tabs[self.active_tab].clone();
        match &current_tab {
            Tab::DirectMessage(peer_id) => {
                let peer_id = peer_id.clone();
                let peer_name = self.get_peer_display_name(&peer_id);

                // Send screen share request to peer
                let req = PlainMessage::screen_share_request(self.own_id.clone());
                let _ = msg_tx.send(OutgoingMessage::Direct {
                    target_id: peer_id.clone(),
                    message: req,
                });

                self.status = format!("🖥️  Requesting screen share with {}...", peer_name);
                self.add_system_message(
                    &current_tab,
                    format!("Requesting to share screen with {}...", peer_name),
                );
            }
            _ => {
                self.status = "Screen sharing works in DM tabs only.".to_string();
            }
        }
    }

    pub(crate) fn handle_stop_share_command(
        &mut self,
        msg_tx: &mut mpsc::UnboundedSender<OutgoingMessage>,
    ) {
        if let Some(target_id) = self.screen_share_target.take() {
            // Stop capture
            if let Some(ref capture) = self.screen_capture {
                capture.stop();
            }
            self.screen_capture = None;
            self.screen_capture_rx = None;
            self.screen_frame = None;
            self.screen_protocol = None;
            self.screen_view_active = false;

            // Notify peer
            let stop_msg = PlainMessage::screen_share_stop(self.own_id.clone());
            let _ = msg_tx.send(OutgoingMessage::Direct {
                target_id: target_id.clone(),
                message: stop_msg,
            });

            let peer_name = self.get_peer_display_name(&target_id);
            self.status = format!("Screen sharing with {} stopped", peer_name);
            let dm_tab = Tab::DirectMessage(target_id);
            self.add_system_message(&dm_tab, "Screen sharing stopped".to_string());
        } else if let Some(from_id) = self.screen_viewer_from.take() {
            // Stop viewing
            self.screen_frame = None;
            self.screen_protocol = None;
            self.screen_view_active = false;

            // Notify peer we stopped watching
            let stop_msg = PlainMessage::screen_share_stop(self.own_id.clone());
            let _ = msg_tx.send(OutgoingMessage::Direct {
                target_id: from_id.clone(),
                message: stop_msg,
            });

            let peer_name = self.get_peer_display_name(&from_id);
            self.status = format!("Stopped viewing {}'s screen", peer_name);
        } else {
            self.status = "Not currently sharing or viewing.".to_string();
        }
    }

    pub(crate) fn handle_accept_screen_command(
        &mut self,
        msg_tx: &mut mpsc::UnboundedSender<OutgoingMessage>,
    ) {
        let peer_id = match self.pending_screen_share_from.take() {
            Some(id) => id,
            None => {
                self.status = "No incoming screen share request.".to_string();
                return;
            }
        };

        let peer_name = self.get_peer_display_name(&peer_id);

        // Send acceptance
        let accept_msg = PlainMessage::screen_share_accept(self.own_id.clone(), true);
        let _ = msg_tx.send(OutgoingMessage::Direct {
            target_id: peer_id.clone(),
            message: accept_msg,
        });

        // Set up to receive frames — screen_view_active will be set on first frame
        self.screen_viewer_from = Some(peer_id.clone());
        self.status = format!("🖥️  Waiting for {}'s screen... (F5 to toggle view)", peer_name);
        let dm_tab = Tab::DirectMessage(peer_id);
        self.add_system_message(&dm_tab, format!("🖥️  Accepted screen share — waiting for frames..."));
    }

    pub(crate) fn handle_reject_screen_command(
        &mut self,
        msg_tx: &mut mpsc::UnboundedSender<OutgoingMessage>,
    ) {
        let peer_id = match self.pending_screen_share_from.take() {
            Some(id) => id,
            None => {
                self.status = "No incoming screen share request.".to_string();
                return;
            }
        };

        let peer_name = self.get_peer_display_name(&peer_id);
        let reject_msg = PlainMessage::screen_share_accept(self.own_id.clone(), false);
        let _ = msg_tx.send(OutgoingMessage::Direct {
            target_id: peer_id.clone(),
            message: reject_msg,
        });

        self.status = format!("Rejected screen share from {}", peer_name);
        let dm_tab = Tab::DirectMessage(peer_id);
        self.add_system_message(&dm_tab, format!("Rejected screen share from {}", peer_name));
    }

    /// Handle incoming screen share request
    pub(crate) fn handle_incoming_screen_request(&mut self, msg: &PlainMessage) {
        let peer_name = self.get_peer_display_name(&msg.sender);
        self.pending_screen_share_from = Some(msg.sender.clone());
        self.status = format!(
            "🖥️  {} wants to share their screen — /accept-screen or /reject-screen",
            peer_name
        );

        let dm_tab = Tab::DirectMessage(msg.sender.clone());
        self.ensure_tab(&dm_tab);
        let sys_msg = PlainMessage::system(
            msg.sender.clone(),
            format!(
                "🖥️  {} wants to share their screen — /accept-screen or /reject-screen",
                peer_name
            ),
        );
        self.messages
            .entry(dm_tab)
            .or_insert_with(Vec::new)
            .push(sys_msg);
    }

    /// Handle screen share accept/reject response
    pub(crate) fn handle_screen_share_response(&mut self, msg: &PlainMessage, accept: bool) {
        let peer_name = self.get_peer_display_name(&msg.sender);
        let dm_tab = Tab::DirectMessage(msg.sender.clone());

        if accept {
            // Peer accepted — start screen capture
            match ScreenCapture::start() {
                Ok(mut capture) => {
                    self.screen_capture_rx = capture.take_frame_rx();
                    self.screen_capture = Some(capture);
                    self.screen_share_target = Some(msg.sender.clone());
                    self.status = format!(
                        "🖥️  Sharing screen with {} | /stop-share to end | F5 toggle view",
                        peer_name
                    );
                    self.add_system_message(
                        &dm_tab,
                        format!("🖥️  Now sharing screen with {}", peer_name),
                    );
                }
                Err(e) => {
                    self.status = format!("❌ Failed to start screen capture: {}", e);
                    self.add_system_message(&dm_tab, format!("❌ Screen capture failed: {}", e));
                }
            }
        } else {
            self.status = format!("{} rejected the screen share", peer_name);
            self.add_system_message(&dm_tab, format!("{} rejected the screen share", peer_name));
        }
    }

    /// Handle screen share stop notification from peer
    pub(crate) fn handle_screen_share_stop(&mut self, msg: &PlainMessage) {
        let peer_name = self.get_peer_display_name(&msg.sender);

        // If we were sharing TO this peer, stop capture
        if self.screen_share_target.as_ref() == Some(&msg.sender) {
            if let Some(ref capture) = self.screen_capture {
                capture.stop();
            }
            self.screen_capture = None;
            self.screen_capture_rx = None;
            self.screen_share_target = None;
            self.screen_frame = None;
            self.screen_protocol = None;
            self.screen_view_active = false;
            self.status = format!("{} stopped watching your screen", peer_name);
            let dm_tab = Tab::DirectMessage(msg.sender.clone());
            self.add_system_message(&dm_tab, format!("{} stopped watching", peer_name));
        }

        // If we were viewing this peer's screen, stop
        if self.screen_viewer_from.as_ref() == Some(&msg.sender) {
            self.screen_viewer_from = None;
            self.screen_frame = None;
            self.screen_protocol = None;
            self.screen_view_active = false;
            self.status = format!("{} stopped sharing their screen", peer_name);
            let dm_tab = Tab::DirectMessage(msg.sender.clone());
            self.add_system_message(
                &dm_tab,
                format!("{} stopped sharing their screen", peer_name),
            );
        }
    }
}
