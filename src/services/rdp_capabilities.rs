//! RDP capability definitions
//!
//! Defines the RDP-side capability representations that Wayland
//! features map to during service advertisement.

use serde::{Deserialize, Serialize};

/// RDP capability that can be advertised to clients
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum RdpCapability {
    /// EGFX Graphics Pipeline codec support
    EgfxCodec {
        /// AVC444 (H.264 + chroma) supported
        avc444: bool,
        /// AVC420 (H.264 standard) supported
        avc420: bool,
        /// RemoteFX (RFX) supported
        remotefx: bool,
        /// Progressive codec supported
        progressive: bool,
    },

    /// Desktop composition capability
    DesktopComposition {
        /// Multi-monitor supported
        multi_mon: bool,
        /// Maximum monitors
        max_monitors: u32,
        /// Desktop scaling supported
        scaling: bool,
    },

    /// Cursor channel capability
    CursorChannel {
        /// Metadata (position-only) mode
        metadata: bool,
        /// Large cursor support (up to 256x256)
        large_cursor: bool,
        /// Color cursor depth
        color_depth: u8,
    },

    /// Extended clipboard capability
    ClipboardExtended {
        /// File copy supported
        file_copy: bool,
        /// Maximum transfer size
        max_size_bytes: u64,
        /// Supported formats
        formats: Vec<String>,
    },

    /// Input capability
    InputCapability {
        /// Keyboard input
        keyboard: bool,
        /// Mouse input
        mouse: bool,
        /// Touch input
        touch: bool,
        /// Unicode input
        unicode: bool,
    },

    /// Surface management
    SurfaceManagement {
        /// Maximum surfaces
        max_surfaces: u32,
        /// Surface commands version
        surface_commands_version: u32,
    },

    /// Frame acknowledgment
    FrameAcknowledge {
        /// Enabled
        enabled: bool,
        /// Maximum unacknowledged frames
        max_unacked: u32,
    },

    /// Custom capability for future extensions
    Custom {
        /// Capability name
        name: String,
        /// Version number
        version: u32,
        /// Additional properties
        properties: Vec<(String, String)>,
    },
}

impl RdpCapability {
    /// Create EGFX capability with AVC420 only
    pub fn egfx_avc420() -> Self {
        Self::EgfxCodec {
            avc444: false,
            avc420: true,
            remotefx: false,
            progressive: false,
        }
    }

    /// Create EGFX capability with full codec support
    pub fn egfx_full() -> Self {
        Self::EgfxCodec {
            avc444: true,
            avc420: true,
            remotefx: true,
            progressive: true,
        }
    }

    /// Create cursor channel with metadata support
    pub fn cursor_metadata() -> Self {
        Self::CursorChannel {
            metadata: true,
            large_cursor: true,
            color_depth: 32,
        }
    }

    /// Create cursor channel without metadata (painted only)
    pub fn cursor_painted() -> Self {
        Self::CursorChannel {
            metadata: false,
            large_cursor: true,
            color_depth: 32,
        }
    }

    /// Create input capability
    pub fn input_full() -> Self {
        Self::InputCapability {
            keyboard: true,
            mouse: true,
            touch: false,
            unicode: true,
        }
    }

    /// Create clipboard capability
    pub fn clipboard_standard(max_size: u64) -> Self {
        Self::ClipboardExtended {
            file_copy: false,
            max_size_bytes: max_size,
            formats: vec![
                "text/plain".to_string(),
                "text/html".to_string(),
                "image/png".to_string(),
            ],
        }
    }

    /// Get short name for logging
    pub fn short_name(&self) -> &'static str {
        match self {
            Self::EgfxCodec { .. } => "egfx",
            Self::DesktopComposition { .. } => "desktop",
            Self::CursorChannel { .. } => "cursor",
            Self::ClipboardExtended { .. } => "clipboard",
            Self::InputCapability { .. } => "input",
            Self::SurfaceManagement { .. } => "surface",
            Self::FrameAcknowledge { .. } => "frame-ack",
            Self::Custom { name, .. } => {
                // Return a static str, we can't return the name directly
                // This is a limitation, but custom caps are rare
                "custom"
            }
        }
    }
}

impl std::fmt::Display for RdpCapability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EgfxCodec {
                avc444,
                avc420,
                remotefx,
                ..
            } => {
                let mut codecs = Vec::new();
                if *avc444 {
                    codecs.push("AVC444");
                }
                if *avc420 {
                    codecs.push("AVC420");
                }
                if *remotefx {
                    codecs.push("RFX");
                }
                write!(f, "EGFX[{}]", codecs.join(","))
            }
            Self::DesktopComposition {
                multi_mon,
                max_monitors,
                ..
            } => {
                if *multi_mon {
                    write!(f, "Desktop[multi-mon, max={}]", max_monitors)
                } else {
                    write!(f, "Desktop[single]")
                }
            }
            Self::CursorChannel { metadata, .. } => {
                if *metadata {
                    write!(f, "Cursor[metadata]")
                } else {
                    write!(f, "Cursor[painted]")
                }
            }
            Self::ClipboardExtended {
                file_copy,
                max_size_bytes,
                ..
            } => {
                let size_mb = max_size_bytes / (1024 * 1024);
                if *file_copy {
                    write!(f, "Clipboard[files, {}MB]", size_mb)
                } else {
                    write!(f, "Clipboard[text, {}MB]", size_mb)
                }
            }
            Self::InputCapability {
                keyboard,
                mouse,
                touch,
                ..
            } => {
                let mut inputs = Vec::new();
                if *keyboard {
                    inputs.push("kbd");
                }
                if *mouse {
                    inputs.push("mouse");
                }
                if *touch {
                    inputs.push("touch");
                }
                write!(f, "Input[{}]", inputs.join(","))
            }
            Self::SurfaceManagement { max_surfaces, .. } => {
                write!(f, "Surface[max={}]", max_surfaces)
            }
            Self::FrameAcknowledge { max_unacked, .. } => {
                write!(f, "FrameAck[max={}]", max_unacked)
            }
            Self::Custom { name, version, .. } => {
                write!(f, "Custom[{} v{}]", name, version)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_egfx_presets() {
        let avc420 = RdpCapability::egfx_avc420();
        if let RdpCapability::EgfxCodec {
            avc444,
            avc420: has_420,
            ..
        } = avc420
        {
            assert!(!avc444);
            assert!(has_420);
        }

        let full = RdpCapability::egfx_full();
        if let RdpCapability::EgfxCodec {
            avc444,
            avc420,
            remotefx,
            ..
        } = full
        {
            assert!(avc444);
            assert!(avc420);
            assert!(remotefx);
        }
    }

    #[test]
    fn test_display() {
        let egfx = RdpCapability::egfx_full();
        assert!(egfx.to_string().contains("AVC444"));

        let cursor = RdpCapability::cursor_metadata();
        assert!(cursor.to_string().contains("metadata"));
    }
}
