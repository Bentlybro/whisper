use std::path::PathBuf;
use tokio::sync::mpsc;

use crate::client::OutgoingMessage;
use crate::protocol::{FileChunk, FileOffer, PlainMessage};

use super::helpers::expand_path;
use super::types::{ActiveTransfer, OutgoingTransfer, PendingFileOffer, Tab, FILE_CHUNK_SIZE};
use super::ChatUI;

impl ChatUI {
    pub(crate) fn handle_share_command(&mut self, filepath: &str, msg_tx: &mut mpsc::UnboundedSender<OutgoingMessage>) {
        self.status = format!("Reading file: {}...", filepath);

        if self.peers.is_empty() {
            self.status = "No peers connected to share with".to_string();
            return;
        }

        let path = expand_path(filepath);

        let file_data = match std::fs::read(&path) {
            Ok(data) => data,
            Err(e) => {
                self.status = format!("Failed to read file: {}", e);
                return;
            }
        };

        let filename = match path.file_name() {
            Some(name) => name.to_string_lossy().to_string(),
            None => {
                self.status = "Invalid file path".to_string();
                return;
            }
        };

        let checksum = blake3::hash(&file_data).to_hex().to_string();
        let total_chunks = ((file_data.len() + FILE_CHUNK_SIZE - 1) / FILE_CHUNK_SIZE) as u32;
        let file_id = format!("{:x}", rand::random::<u64>());

        let offer = FileOffer {
            file_id: file_id.clone(),
            filename: filename.clone(),
            size: file_data.len() as u64,
            checksum,
            total_chunks,
        };

        let current_tab = &self.tabs[self.active_tab];
        let (is_direct, target_peer) = match current_tab {
            Tab::Global => (false, String::new()),
            Tab::DirectMessage(peer_id) => (true, peer_id.clone()),
            Tab::Group(_group_id) => (false, String::new()),
        };

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

        self.outgoing_transfers.insert(file_id.clone(), OutgoingTransfer {
            offer: offer.clone(),
            file_data,
            target_peer,
            chunks_sent: 0,
            is_direct,
        });

        self.status = format!("Offering file: {} ({})", filename, Self::format_size(offer.size));
    }

    pub(crate) fn handle_accept_command(&mut self, save_path: &str, msg_tx: &mut mpsc::UnboundedSender<OutgoingMessage>) {
        let current_tab = &self.tabs[self.active_tab];

        let offer_to_accept = self.pending_offers.iter()
            .find(|(_, pending)| &pending.tab == current_tab)
            .map(|(id, pending)| (id.clone(), pending.clone()));

        if let Some((file_id, pending)) = offer_to_accept {
            let save_dir = expand_path(save_path);

            let full_path = if save_dir.is_dir() || save_path.ends_with('/') || save_path == "." {
                save_dir.join(&pending.offer.filename)
            } else {
                save_dir
            };

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
                Tab::Group(_group_id) => {
                    let _ = msg_tx.send(OutgoingMessage::Direct {
                        target_id: pending.from_peer.clone(),
                        message: response_msg,
                    });
                }
            }

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

    pub(crate) fn handle_reject_command(&mut self, msg_tx: &mut mpsc::UnboundedSender<OutgoingMessage>) {
        let current_tab = &self.tabs[self.active_tab];

        let offer_to_reject = self.pending_offers.iter()
            .find(|(_, pending)| &pending.tab == current_tab)
            .map(|(id, pending)| (id.clone(), pending.clone()));

        if let Some((file_id, pending)) = offer_to_reject {
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

    pub(crate) fn handle_file_offer(&mut self, msg: PlainMessage) {
        if let Some(offer) = msg.file_offer {
            let file_id = offer.file_id.clone();
            let sender_name = self.get_peer_display_name(&msg.sender);

            let tab = if let Some(ref group_id) = msg.group_id {
                Tab::Group(group_id.clone())
            } else if msg.direct {
                Tab::DirectMessage(msg.sender.clone())
            } else {
                Tab::Global
            };

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

    pub(crate) fn handle_file_chunk(&mut self, msg: PlainMessage) {
        if let Some(chunk) = msg.file_chunk {
            let file_id = &chunk.file_id;

            if let Some(transfer) = self.active_transfers.get_mut(file_id) {
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

                        if transfer.chunks_done == transfer.offer.total_chunks {
                            self.finalize_transfer(file_id);
                        }
                    }
                }
            }
        }
    }

    pub(crate) fn handle_file_response(&mut self, msg: PlainMessage, accept: bool, msg_tx: &mut mpsc::UnboundedSender<OutgoingMessage>) {
        let file_id = &msg.content;

        if !accept {
            if let Some(transfer) = self.outgoing_transfers.remove(file_id) {
                self.status = format!("File rejected: {}", transfer.offer.filename);
            }
            return;
        }

        let sender_name = self.get_peer_display_name(&msg.sender);
        if let Some(transfer) = self.outgoing_transfers.get_mut(file_id) {
            self.status = format!("{} accepted {}. Sending...", sender_name, transfer.offer.filename);

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

    pub(crate) fn finalize_transfer(&mut self, file_id: &str) {
        if let Some(transfer) = self.active_transfers.remove(file_id) {
            let mut file_data = Vec::new();
            for chunk_opt in &transfer.chunks_received {
                if let Some(chunk) = chunk_opt {
                    file_data.extend_from_slice(chunk);
                } else {
                    self.status = format!("Error: Missing chunks for {}", transfer.offer.filename);
                    return;
                }
            }

            let actual_checksum = blake3::hash(&file_data).to_hex().to_string();
            if actual_checksum != transfer.offer.checksum {
                self.status = format!("Error: Checksum mismatch for {}", transfer.offer.filename);
                return;
            }

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

    pub(crate) fn format_size(bytes: u64) -> String {
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
