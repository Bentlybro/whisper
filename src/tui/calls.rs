use tokio::sync::mpsc;

use crate::audio::AudioPipeline;
use crate::client::OutgoingMessage;
use crate::protocol::PlainMessage;

use super::helpers::format_duration;
use super::types::{CallState, CallType, Tab};
use super::ChatUI;

impl ChatUI {
    pub(crate) fn handle_call_command(&mut self, msg_tx: &mut mpsc::UnboundedSender<OutgoingMessage>) {
        let current_tab = self.tabs[self.active_tab].clone();

        if self.active_call.is_some() {
            self.status = "Already in a call. Use /hangup first.".to_string();
            return;
        }

        match &current_tab {
            Tab::DirectMessage(peer_id) => {
                let peer_id = peer_id.clone();
                let call_req = PlainMessage::call_request(self.own_id.clone());
                let _ = msg_tx.send(OutgoingMessage::Direct {
                    target_id: peer_id.clone(),
                    message: call_req,
                });

                let peer_name = self.get_peer_display_name(&peer_id);
                self.status = format!("📞 Calling {}...", peer_name);
                self.add_system_message(&current_tab, format!("Calling {}...", peer_name));
            }
            Tab::Group(group_id) => {
                let group_id = group_id.clone();
                if let Some(group) = self.groups.get(&group_id) {
                    if group.members.is_empty() {
                        self.status = "No members in group to call".to_string();
                        return;
                    }

                    let group_name = group.name.clone();
                    let member_ids = group.members.clone();

                    let mut call_req = PlainMessage::call_request(self.own_id.clone());
                    call_req.group_id = Some(group_id.clone());
                    let _ = msg_tx.send(OutgoingMessage::Group {
                        group_id: group_id.clone(),
                        member_ids,
                        message: call_req,
                    });

                    self.start_audio_call_group(group_id.clone());

                    self.status = format!("📞 Starting group call in {}...", group_name);
                    self.add_system_message(&current_tab, format!("📞 Starting group call in {}", group_name));
                } else {
                    self.status = "Group not found".to_string();
                }
            }
            _ => {
                self.status = "Voice calls work in DM or Group tabs.".to_string();
            }
        }
    }

    pub(crate) fn handle_accept_call_command(&mut self, msg_tx: &mut mpsc::UnboundedSender<OutgoingMessage>) {
        if self.active_call.is_some() {
            self.status = "Already in a call. Use /hangup first.".to_string();
            return;
        }

        // Check for pending group call first, then DM call
        if let Some((group_id, _initiator_id)) = self.pending_group_call.take() {
            if let Some(group) = self.groups.get(&group_id) {
                let member_ids = group.members.clone();
                let mut accept_msg = PlainMessage::call_accept(self.own_id.clone(), true);
                accept_msg.group_id = Some(group_id.clone());
                let _ = msg_tx.send(OutgoingMessage::Group {
                    group_id: group_id.clone(),
                    member_ids,
                    message: accept_msg,
                });
            }

            self.start_audio_call_group(group_id.clone());

            let group_name = self.group_name(&group_id);
            let group_tab = Tab::Group(group_id);
            self.add_system_message(&group_tab, format!("🔊 Joined group call in {}", group_name));
        } else if let Some(peer_id) = self.pending_call_from.take() {
            let accept_msg = PlainMessage::call_accept(self.own_id.clone(), true);
            let _ = msg_tx.send(OutgoingMessage::Direct {
                target_id: peer_id.clone(),
                message: accept_msg,
            });

            self.start_audio_call(peer_id.clone());
        } else {
            self.status = "No incoming call to accept.".to_string();
        }
    }

    pub(crate) fn handle_reject_call_command(&mut self, msg_tx: &mut mpsc::UnboundedSender<OutgoingMessage>) {
        if let Some((group_id, _initiator_id)) = self.pending_group_call.take() {
            if let Some(group) = self.groups.get(&group_id) {
                let member_ids = group.members.clone();
                let mut reject_msg = PlainMessage::call_accept(self.own_id.clone(), false);
                reject_msg.group_id = Some(group_id.clone());
                let _ = msg_tx.send(OutgoingMessage::Group {
                    group_id: group_id.clone(),
                    member_ids,
                    message: reject_msg,
                });
            }

            let group_name = self.group_name(&group_id);
            self.status = format!("Rejected group call in {}", group_name);

            let group_tab = Tab::Group(group_id);
            self.add_system_message(&group_tab, format!("Declined group call in {}", group_name));
        } else if let Some(peer_id) = self.pending_call_from.take() {
            let reject_msg = PlainMessage::call_accept(self.own_id.clone(), false);
            let _ = msg_tx.send(OutgoingMessage::Direct {
                target_id: peer_id.clone(),
                message: reject_msg,
            });

            let peer_name = self.get_peer_display_name(&peer_id);
            self.status = format!("Rejected call from {}", peer_name);

            let dm_tab = Tab::DirectMessage(peer_id.clone());
            self.add_system_message(&dm_tab, format!("Rejected call from {}", peer_name));
        } else {
            self.status = "No incoming call to reject.".to_string();
        }
    }

    pub(crate) fn handle_hangup_command(&mut self, msg_tx: &mut mpsc::UnboundedSender<OutgoingMessage>) {
        let call = match self.active_call.take() {
            Some(c) => c,
            None => {
                self.status = "Not in a call.".to_string();
                return;
            }
        };

        match &call.call_type {
            CallType::Direct(peer_id) => {
                let hangup_msg = PlainMessage::call_hangup(self.own_id.clone());
                let _ = msg_tx.send(OutgoingMessage::Direct {
                    target_id: peer_id.clone(),
                    message: hangup_msg,
                });
            }
            CallType::Group { group_id } => {
                if let Some(group) = self.groups.get(group_id) {
                    let member_ids = group.members.clone();
                    let mut hangup_msg = PlainMessage::call_hangup(self.own_id.clone());
                    hangup_msg.group_id = Some(group_id.clone());
                    let _ = msg_tx.send(OutgoingMessage::Group {
                        group_id: group_id.clone(),
                        member_ids,
                        message: hangup_msg,
                    });
                }
            }
        }

        self.stop_audio_call(&call);
    }

    pub(crate) fn handle_incoming_call_request(&mut self, msg: &PlainMessage, _msg_tx: &mut mpsc::UnboundedSender<OutgoingMessage>) {
        let peer_name = self.get_peer_display_name(&msg.sender);

        if self.active_call.is_some() {
            self.status = format!("📞 Missed call from {} (already in a call)", peer_name);
            return;
        }

        if let Some(ref group_id) = msg.group_id {
            let group_name = self.group_name(group_id);

            self.pending_group_call = Some((group_id.clone(), msg.sender.clone()));
            self.status = format!("📞 Incoming group call in {} from {} — /accept-call or /reject-call", group_name, peer_name);

            let group_tab = Tab::Group(group_id.clone());
            self.ensure_tab(&group_tab);

            // Use sender's ID for system message attribution
            let sys_msg = PlainMessage::system(
                msg.sender.clone(),
                format!("📞 {} started a group call — /accept-call or /reject-call", peer_name),
            );
            self.messages.entry(group_tab).or_insert_with(Vec::new).push(sys_msg);
        } else {
            self.pending_call_from = Some(msg.sender.clone());
            self.status = format!("📞 Incoming call from {} — /accept-call or /reject-call", peer_name);

            let dm_tab = Tab::DirectMessage(msg.sender.clone());
            self.ensure_tab(&dm_tab);

            let sys_msg = PlainMessage::system(
                msg.sender.clone(),
                format!("📞 Incoming call from {} — /accept-call or /reject-call", peer_name),
            );
            self.messages.entry(dm_tab).or_insert_with(Vec::new).push(sys_msg);
        }
    }

    pub(crate) fn handle_call_response(&mut self, msg: &PlainMessage, accept: bool, _msg_tx: &mut mpsc::UnboundedSender<OutgoingMessage>) {
        let peer_name = self.get_peer_display_name(&msg.sender);

        if let Some(ref group_id) = msg.group_id {
            let group_name = self.group_name(group_id);
            let group_tab = Tab::Group(group_id.clone());

            if accept {
                let sys_msg = PlainMessage::system(
                    msg.sender.clone(),
                    format!("🔊 {} joined the group call", peer_name),
                );
                self.messages.entry(group_tab).or_insert_with(Vec::new).push(sys_msg);
                self.status = format!("{} joined the call in {}", peer_name, group_name);
            } else {
                let sys_msg = PlainMessage::system(
                    msg.sender.clone(),
                    format!("{} declined the group call", peer_name),
                );
                self.messages.entry(group_tab).or_insert_with(Vec::new).push(sys_msg);
            }
        } else {
            let dm_tab = Tab::DirectMessage(msg.sender.clone());

            if accept {
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
    }

    pub(crate) fn handle_remote_hangup(&mut self, msg: &PlainMessage, _msg_tx: &mut mpsc::UnboundedSender<OutgoingMessage>) {
        let peer_name = self.get_peer_display_name(&msg.sender);

        if let Some(ref group_id) = msg.group_id {
            let group_tab = Tab::Group(group_id.clone());
            let sys_msg = PlainMessage::system(
                msg.sender.clone(),
                format!("📵 {} left the group call", peer_name),
            );
            self.messages.entry(group_tab).or_insert_with(Vec::new).push(sys_msg);
        } else {
            if let Some(ref call) = self.active_call {
                match &call.call_type {
                    CallType::Direct(peer_id) if *peer_id == msg.sender => {
                        let call = self.active_call.take().unwrap();
                        self.stop_audio_call(&call);
                    }
                    _ => {}
                }
            }
        }
    }

    pub(crate) fn start_audio_call(&mut self, peer_id: String) {
        let peer_name = self.get_peer_display_name(&peer_id);

        match AudioPipeline::start() {
            Ok(mut pipeline) => {
                self.audio_capture_rx = pipeline.take_capture_rx();
                self.audio_pipeline = Some(pipeline);
                self.active_call = Some(CallState {
                    call_type: CallType::Direct(peer_id.clone()),
                    start_time: chrono::Utc::now(),
                    muted: false,
                });
                self.status = format!("🔊 In call with {} | /mute to toggle mic | /hangup to end", peer_name);

                let dm_tab = Tab::DirectMessage(peer_id);
                self.add_system_message(&dm_tab, format!("🔊 Voice call started with {}", peer_name));
            }
            Err(e) => {
                self.status = format!("❌ Failed to start audio: {}", e);
                let dm_tab = Tab::DirectMessage(peer_id);
                self.add_system_message(&dm_tab, format!("❌ Failed to start audio: {}", e));
            }
        }
    }

    pub(crate) fn stop_audio_call(&mut self, call: &CallState) {
        let duration = chrono::Utc::now() - call.start_time;
        let duration_str = format_duration(duration);

        if let Some(ref pipeline) = self.audio_pipeline {
            pipeline.stop();
        }
        self.audio_pipeline = None;
        self.audio_capture_rx = None;

        match &call.call_type {
            CallType::Direct(peer_id) => {
                let peer_name = self.get_peer_display_name(peer_id);
                self.status = format!("Call with {} ended ({})", peer_name, duration_str);

                let dm_tab = Tab::DirectMessage(peer_id.clone());
                self.add_system_message(&dm_tab, format!("📵 Call ended with {} ({})", peer_name, duration_str));
            }
            CallType::Group { group_id } => {
                let group_name = self.group_name(group_id);
                self.status = format!("Left group call in {} ({})", group_name, duration_str);

                let group_tab = Tab::Group(group_id.clone());
                self.add_system_message(&group_tab, format!("📵 Left group call in {} ({})", group_name, duration_str));
            }
        }
    }

    pub(crate) fn start_audio_call_group(&mut self, group_id: String) {
        let group_name = self.group_name(&group_id);

        match AudioPipeline::start() {
            Ok(mut pipeline) => {
                self.audio_capture_rx = pipeline.take_capture_rx();
                self.audio_pipeline = Some(pipeline);
                self.active_call = Some(CallState {
                    call_type: CallType::Group { group_id: group_id.clone() },
                    start_time: chrono::Utc::now(),
                    muted: false,
                });
                self.status = format!("🔊 In group call: {} | /mute to toggle mic | /hangup to leave", group_name);
            }
            Err(e) => {
                self.status = format!("❌ Failed to start audio: {}", e);
                let group_tab = Tab::Group(group_id);
                self.add_system_message(&group_tab, format!("❌ Failed to start audio: {}", e));
            }
        }
    }
}
