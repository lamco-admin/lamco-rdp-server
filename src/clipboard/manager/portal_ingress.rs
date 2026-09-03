//! Portal-ingress clipboard handlers (Wayland portal -> orchestrator)
//!
//! Split out of `manager/mod.rs` (architecture audit M3). These `impl`
//! blocks contribute to the same `ClipboardOrchestrator` type; `use super::*`
//! inherits the parent module's imports and private-field access.

use std::{collections::HashMap, sync::Arc};

use lamco_clipboard_core::{ClipboardFormat, FormatConverter};
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

use super::{
    ClipboardOrchestrator, ServerEventSender, SharedClipboardProvider, lookup_format_id_for_mime,
};
use crate::clipboard::{FormatConverterExt, error::Result, sync::SyncManager};

impl ClipboardOrchestrator {
    /// Handle Portal format announcement (Linux → Windows)
    ///
    /// `force=true` from D-Bus extension overrides RDP ownership; `force=false` may be blocked.
    #[expect(
        clippy::too_many_arguments,
        reason = "orchestrator handler with shared state refs"
    )]
    pub(super) async fn handle_portal_formats(
        mime_types: Vec<String>,
        force: bool,
        converter: &FormatConverter,
        sync_manager: &Arc<RwLock<SyncManager>>,
        server_event_sender: &ServerEventSender,
        local_advertised_formats: &Arc<RwLock<Vec<ClipboardFormat>>>,
        current_rdp_formats: &Arc<RwLock<Vec<ClipboardFormat>>>,
        clipboard_provider: &SharedClipboardProvider,
        last_reannounce_time: &Arc<RwLock<Option<std::time::SystemTime>>>,
        reannounce_count: &Arc<RwLock<HashMap<Vec<u32>, u32>>>,
        file_transfer_backend: &Arc<
            RwLock<Box<dyn crate::clipboard::file_transfer::FileTransferBackend>>,
        >,
        rdp_ready: &Arc<std::sync::atomic::AtomicBool>,
        remote_owns_selection: &Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<()> {
        use std::time::{Duration, SystemTime};

        info!(
            "handle_portal_formats called with {} MIME types (force={}): {:?}",
            mime_types.len(),
            force,
            mime_types
        );

        let sync_decision = {
            let mut mgr = sync_manager.write().await;
            mgr.handle_portal_formats(mime_types.clone(), force)
        };

        match sync_decision {
            crate::clipboard::sync::PortalSyncDecision::Allow => {
                // Normal Linux → Windows sync
                debug!("Sync decision: Allow - proceeding with normal sync");
                // A real local copy, so the selection is no longer ours and the
                // compositor will answer reads for it again. This must happen
                // here rather than on entry: Mutter echoes SelectionOwnerChanged
                // about our own SetSelection within a millisecond of it, and
                // clearing on that echo handed the next read straight back to
                // the compositor, which then refused it.
                if remote_owns_selection.swap(false, std::sync::atomic::Ordering::SeqCst) {
                    debug!(
                        "Local application took clipboard ownership; compositor reads re-enabled"
                    );
                }
            }

            crate::clipboard::sync::PortalSyncDecision::Block => {
                debug!("Sync decision: Block - skipping Portal formats");
                return Ok(());
            }

            crate::clipboard::sync::PortalSyncDecision::KlipperReannounce => {
                // Klipper took over clipboard - re-announce RDP formats to reclaim ownership
                info!("┌─ Klipper Takeover Mitigation ─────────────────────────────");
                info!("│ Klipper has taken clipboard ownership");

                // GUARD 1: Time-based (prevent rapid reannouncements)
                let time_ok = {
                    let last_time = last_reannounce_time.read().await;
                    match *last_time {
                        Some(t) => {
                            let elapsed = SystemTime::now()
                                .duration_since(t)
                                .unwrap_or(Duration::from_secs(999))
                                .as_millis();

                            if elapsed < 500 {
                                warn!("│ SKIP: Reannounced {}ms ago (< 500ms guard)", elapsed);
                                info!(
                                    "└────────────────────────────────────────────────────────────"
                                );
                                return Ok(());
                            } else {
                                debug!("│ Time guard OK: {}ms since last reannounce", elapsed);
                                true
                            }
                        }
                        None => {
                            debug!("│ First reannounce - no time guard");
                            true
                        }
                    }
                };

                if !time_ok {
                    info!("└────────────────────────────────────────────────────────────");
                    return Ok(());
                }

                // GUARD 2: Count-based (max 2 reannouncements per RDP format list)
                let count_ok = {
                    let stored_formats = current_rdp_formats.read().await;
                    let mut format_ids: Vec<u32> = stored_formats.iter().map(|f| f.id).collect();
                    format_ids.sort_unstable(); // Ensure consistent ordering for HashMap key

                    let mut counts = reannounce_count.write().await;
                    let count = counts.entry(format_ids).or_insert(0);

                    if *count >= 2 {
                        warn!(
                            "│ SKIP: Already reannounced {} times for this RDP copy",
                            count
                        );
                        warn!("│ Accepting Klipper ownership to prevent infinite loop");
                        info!("└────────────────────────────────────────────────────────────");
                        return Ok(());
                    } else {
                        *count += 1;
                        info!("│ Reannounce attempt #{} (max 2 allowed)", count);
                        true
                    }
                };

                if !count_ok {
                    info!("└────────────────────────────────────────────────────────────");
                    return Ok(());
                }

                // RE-ANNOUNCE: Call SetSelection again with original RDP formats
                let stored = current_rdp_formats.read().await;

                if stored.is_empty() {
                    warn!("│ No RDP formats stored to re-announce");
                    info!("└────────────────────────────────────────────────────────────");
                    return Ok(());
                }

                let reannounce_mimes = converter.rdp_to_mime_types(&stored)?;

                info!(
                    "│ Re-announcing {} RDP formats as {} MIME types",
                    stored.len(),
                    reannounce_mimes.len()
                );
                debug!("│ MIME types: {:?}", reannounce_mimes);

                match *clipboard_provider.read().await {
                    Some(ref provider) => match provider.announce_formats(reannounce_mimes).await {
                        Ok(()) => {
                            info!(
                                "│ SetSelection succeeded via {} provider - ownership reclaimed",
                                provider.name()
                            );
                            *last_reannounce_time.write().await = Some(SystemTime::now());
                        }
                        Err(e) => {
                            error!("│ Provider announce_formats failed: {:#}", e);
                        }
                    },
                    _ => {
                        warn!("│ No clipboard provider available for reannounce");
                    }
                }

                info!("└────────────────────────────────────────────────────────────");
                return Ok(());
            }
        }

        let rdp_formats = converter.mime_to_rdp_formats(&mime_types)?;
        debug!(
            "Converted {} MIME types to {} RDP formats",
            mime_types.len(),
            rdp_formats.len()
        );

        let ironrdp_formats: Vec<ironrdp_cliprdr::pdu::ClipboardFormat> = rdp_formats
            .iter()
            .map(|f| {
                let name = if !f.format_name.is_empty() {
                    Some(ironrdp_cliprdr::pdu::ClipboardFormatName::new(
                        f.format_name.clone(),
                    ))
                } else {
                    None
                };
                ironrdp_cliprdr::pdu::ClipboardFormat {
                    id: ironrdp_cliprdr::pdu::ClipboardFormatId(f.format_id),
                    name,
                }
            })
            .collect();

        {
            let mut advertised = local_advertised_formats.write().await;
            advertised.clear();
            for fmt in &ironrdp_formats {
                advertised.push(ClipboardFormat {
                    id: fmt.id.0,
                    name: fmt.name.as_ref().map(|n| n.value().to_string()),
                });
            }
            debug!(
                "Stored {} advertised formats for data request lookup",
                advertised.len()
            );
        }

        // Defer the announce until the CLIPRDR channel is Ready. Plain
        // SendInitiateCopy is legal pre-Ready, but SendInitiateFileCopy is
        // require_ready-gated in IronRDP and would propagate an error out of the
        // run-loop and drop the connection — the same crash class the paste gate
        // closes. The formats are cached in local_advertised_formats above, so
        // the RdpReady handler re-announces them (file-aware) once Ready.
        if !rdp_ready.load(std::sync::atomic::Ordering::SeqCst) {
            debug!(
                "Local clipboard offer arrived before RDP clipboard Ready — deferring \
                 announce; RdpReady will re-announce the cached formats"
            );
            return Ok(());
        }

        debug!(" Sending FormatList to RDP client:");
        for (idx, fmt) in ironrdp_formats.iter().enumerate() {
            let name_str = fmt
                .name
                .as_ref()
                .map_or("", ironrdp_cliprdr::pdu::ClipboardFormatName::value);
            info!("   Format {}: ID={}, Name={:?}", idx, fmt.id.0, name_str);
        }

        // Linux → Windows file copy: when the local clipboard offers files, record the
        // file list in IronRDP via initiate_file_copy. A plain initiate_copy leaves the
        // list empty, so the client's FileContentsRequest is rejected with "no file list
        // available" (MS-RDPECLIP 3.1.5.4.6) and the paste produces nothing.
        let file_descriptors = if ironrdp_formats.iter().any(|f| {
            f.name
                .as_ref()
                .is_some_and(|n| n.value() == "FileGroupDescriptorW")
        }) {
            Self::prepare_outgoing_file_copy(clipboard_provider, file_transfer_backend).await
        } else {
            None
        };

        let sender_opt = server_event_sender.read().await.clone();
        if let Some(sender) = sender_opt {
            use ironrdp_cliprdr::backend::ClipboardMessage;

            let message = match file_descriptors {
                Some(files) => {
                    info!(
                        "Sending ServerEvent::Clipboard(SendInitiateFileCopy) with {} file(s) to event loop",
                        files.len()
                    );
                    ClipboardMessage::SendInitiateFileCopy(files)
                }
                None => {
                    info!(
                        "Sending ServerEvent::Clipboard(SendInitiateCopy) with {} formats to event loop",
                        ironrdp_formats.len()
                    );
                    ClipboardMessage::SendInitiateCopy(ironrdp_formats)
                }
            };

            match sender.send(ironrdp_server::ServerEvent::Clipboard(message)) {
                Ok(()) => {
                    debug!(" ServerEvent::Clipboard sent successfully to IronRDP event loop");
                }
                Err(e) => {
                    error!("Failed to send ServerEvent::Clipboard: {:?}", e);
                    error!("   This means the event loop channel is closed/dropped!");
                }
            }
        } else {
            warn!("ServerEvent sender not available - cannot announce formats to RDP");
        }

        Ok(())
    }

    /// Read the Linux clipboard's file list and prepare a Linux → Windows file copy:
    /// register the on-disk paths with the transfer backend (so content requests can be
    /// served) and return IronRDP file descriptors for the offer.
    ///
    /// Returns `None` when no resolvable files are present, in which case the caller
    /// falls back to a plain format copy.
    pub(super) async fn prepare_outgoing_file_copy(
        clipboard_provider: &SharedClipboardProvider,
        file_transfer_backend: &Arc<
            RwLock<Box<dyn crate::clipboard::file_transfer::FileTransferBackend>>,
        >,
    ) -> Option<Vec<ironrdp_cliprdr::pdu::FileDescriptor>> {
        use ironrdp_cliprdr::pdu::{ClipboardFileAttributes, FileDescriptor};
        use lamco_clipboard_core::sanitize::parse_file_uris;

        use crate::clipboard::file_transfer::OutgoingFileInfo;

        let uri_data = {
            let guard = clipboard_provider.read().await;
            let provider = guard.as_ref()?;
            match provider.read_data("text/uri-list").await {
                Ok(data) if !data.is_empty() => data,
                _ => provider
                    .read_data("x-special/gnome-copied-files")
                    .await
                    .ok()?,
            }
        };

        let mut paths = parse_file_uris(&uri_data);
        paths.retain(|p| !crate::clipboard::file_transfer::is_stale_foreign_fuse_path(p));
        if paths.is_empty() {
            warn!("File copy announced but no resolvable file URIs on the Linux clipboard");
            return None;
        }

        let mut descriptors = Vec::with_capacity(paths.len());
        let mut outgoing = Vec::with_capacity(paths.len());
        for path in &paths {
            let Some(filename) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else {
                continue;
            };
            if filename.is_empty() {
                continue;
            }

            let meta = tokio::fs::metadata(path).await.ok();
            let size = meta.as_ref().map_or(0, std::fs::Metadata::len);
            let is_dir = meta.as_ref().is_some_and(std::fs::Metadata::is_dir);

            let mut descriptor = FileDescriptor::new(filename.clone());
            descriptor.file_size = Some(size);
            descriptor.attributes = Some(if is_dir {
                ClipboardFileAttributes::DIRECTORY
            } else {
                ClipboardFileAttributes::ARCHIVE
            });

            // list_index must match the descriptor's position so FileContentsRequest
            // indices resolve to the right file; both vecs grow in lockstep.
            let list_index = descriptors.len() as u32;
            descriptors.push(descriptor);
            outgoing.push(OutgoingFileInfo {
                list_index,
                path: path.clone(),
                size,
                filename,
            });
        }

        if descriptors.is_empty() {
            return None;
        }

        file_transfer_backend
            .read()
            .await
            .set_outgoing_files(outgoing);

        info!(
            "File copy: prepared {} file(s) for Linux → Windows transfer",
            descriptors.len()
        );
        Some(descriptors)
    }

    /// Handle Portal data request (Windows → Linux paste initiation)
    ///
    /// When a Linux app pastes while RDP owns the clipboard, Portal sends
    /// SelectionTransfer which becomes PortalDataRequest. We ask the RDP
    /// client to supply the data via SendInitiatePaste.
    pub(super) async fn handle_portal_data_request(
        mime_type: String,
        converter: &FormatConverter,
        _sync_manager: &Arc<RwLock<SyncManager>>,
        server_event_sender: &ServerEventSender,
        current_rdp_formats: &Arc<RwLock<Vec<ClipboardFormat>>>,
    ) -> Result<()> {
        debug!("Portal data request for MIME type: {}", mime_type);

        // Look up the format ID from the actual Windows format list first.
        // This handles runtime-registered formats (FileGroupDescriptorW etc.)
        // whose IDs vary per session.
        let formats = current_rdp_formats.read().await;
        let format_id = if let Some(id) = lookup_format_id_for_mime(&formats, &mime_type) {
            id
        } else {
            // Fall back to static mapping for standard formats
            drop(formats);
            converter.mime_to_format_id(&mime_type)?
        };

        info!(
            "Portal needs data — requesting format {} from RDP client (MIME: {})",
            format_id, mime_type
        );

        let sender_opt = server_event_sender.read().await.clone();
        if let Some(sender) = sender_opt {
            use ironrdp_cliprdr::{backend::ClipboardMessage, pdu::ClipboardFormatId};

            if let Err(e) = sender.send(ironrdp_server::ServerEvent::Clipboard(
                ClipboardMessage::SendInitiatePaste(ClipboardFormatId(format_id)),
            )) {
                error!(
                    "Failed to send SendInitiatePaste for format {}: {:?}",
                    format_id, e
                );
            }
        } else {
            warn!(
                "ServerEvent sender not available — cannot request format {} from RDP client",
                format_id
            );
        }

        Ok(())
    }
}
