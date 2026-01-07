//! # lamco-rdp-server
//!
//! Wayland RDP server for Linux - Portal mode for desktop sharing.
//!
//! This is the main server orchestration crate that integrates:
//! - [`lamco_portal`] - XDG Desktop Portal integration
//! - [`lamco_pipewire`] - PipeWire screen capture
//! - [`lamco_video`] - Video frame processing
//! - [`lamco_rdp_input`] - RDP input event translation
//! - [`lamco_rdp_clipboard`] - RDP clipboard integration
//!
//! # Architecture
//!
//! ```text
//! lamco-rdp-server
//!   ├─> Portal Session (screen capture + input injection permissions)
//!   ├─> PipeWire Manager (video frame capture)
//!   ├─> Display Handler (video streaming to RDP clients)
//!   ├─> Input Handler (keyboard/mouse from RDP clients)
//!   ├─> Clipboard Manager (bidirectional clipboard sync)
//!   └─> IronRDP Server (RDP protocol, TLS, RemoteFX encoding)
//! ```
//!
//! # Data Flow
//!
//! **Video Path:** Portal → PipeWire → Display Handler → IronRDP → Client
//!
//! **Input Path:** Client → IronRDP → Input Handler → Portal → Compositor
//!
//! **Clipboard Path:** Client ↔ IronRDP ↔ Clipboard Manager ↔ Portal ↔ Compositor

#![warn(missing_docs)]
#![warn(clippy::all)]

// =============================================================================
// Server-specific modules (kept in this crate)
// =============================================================================

/// Server configuration
pub mod config;

/// Multi-monitor support
pub mod multimon;

/// Protocol utilities
pub mod protocol;

/// RDP channel management
pub mod rdp;

/// Security and TLS
pub mod security;

/// Main server implementation
pub mod server;

/// Telemetry: diagnostics, metrics, and error formatting
pub mod telemetry;

/// Clipboard orchestration (bridges portal ↔ RDP)
///
/// Bridges `lamco_portal::ClipboardManager` with `lamco_rdp_clipboard::RdpCliprdrFactory`
/// via the `ClipboardSink` trait from `lamco_clipboard_core`.
pub mod clipboard;

/// EGFX (RDP Graphics Pipeline Extension) for H.264 video streaming
///
/// Server-side EGFX channel for hardware-accelerated H.264 encoding.
/// Requires `h264` feature.
///
/// Features: AVC420/AVC444 codecs, surface management, frame acknowledgments.
pub mod egfx;

/// Damage region detection for bandwidth optimization
///
/// Tile-based frame differencing detects changed screen regions,
/// reducing bandwidth 90%+ for static content.
///
/// SIMD-optimized (AVX2/NEON), configurable tile size, automatic region merging.
pub mod damage;

/// Compositor capability probing
///
/// Auto-detects Wayland compositor (GNOME, KDE, Sway, Hyprland) and probes
/// portal capabilities. Eliminates manual per-DE configuration.
pub mod compositor;

/// Performance optimization: adaptive FPS and latency governors
///
/// - **Adaptive FPS**: 5-30 FPS based on screen activity (30-50% CPU savings)
/// - **Latency Governor**: Interactive (<50ms), Balanced (<100ms), Quality (<300ms)
pub mod performance;

/// Cursor handling: metadata, painted, hidden, and predictive modes
///
/// Predictive cursor uses velocity/acceleration tracking to compensate for
/// network latency, making movement feel instant even at 100ms+ RTT.
pub mod cursor;

/// Service Advertisement Registry
///
/// Translates Wayland compositor capabilities to RDP-compatible service
/// advertisements. Enables runtime feature decisions based on availability.
pub mod services;

/// Session Persistence & Unattended Access
///
/// Multi-strategy session persistence for unattended operation:
/// portal restore tokens, Secret Service/TPM storage, deployment auto-detection.
pub mod session;

/// Mutter Direct D-Bus API (GNOME 42+, non-sandboxed only)
///
/// Bypasses XDG Portal for zero-dialog operation on GNOME.
/// Direct PipeWire access, input injection, virtual monitor support.
pub mod mutter;

// =============================================================================
// Re-exports from published lamco crates (for convenience)
// =============================================================================

/// Re-export lamco-portal for portal integration
pub use lamco_portal;

/// Re-export lamco-pipewire for PipeWire integration
pub use lamco_pipewire;

/// Re-export lamco-video for video processing
pub use lamco_video;

/// Re-export lamco-rdp-input for input handling
pub use lamco_rdp_input;

/// Re-export lamco-clipboard-core for clipboard primitives
pub use lamco_clipboard_core;

/// Re-export lamco-rdp-clipboard for RDP clipboard
pub use lamco_rdp_clipboard;

// =============================================================================
// Convenience aliases
// =============================================================================

/// Portal types (convenience re-export)
pub mod portal {
    pub use lamco_portal::{
        ClipboardManager as PortalClipboardManager, PortalConfig, PortalConfigBuilder, PortalError,
        PortalManager, PortalSessionHandle, RemoteDesktopManager, Result as PortalResult,
        ScreenCastManager, SourceType, StreamInfo,
    };
}

/// PipeWire types (convenience re-export)
pub mod pipewire {
    pub use lamco_pipewire::{
        // Multi-stream coordinator types
        MonitorInfo,
        MultiStreamConfig,
        MultiStreamCoordinator,
        // Core manager types
        PipeWireConfig,
        PipeWireConfigBuilder,
        // Connection types
        PipeWireConnection,
        PipeWireError,
        PipeWireManager,
        PipeWireThreadCommand,
        PipeWireThreadManager,
        // Stream types
        PixelFormat,
        Result as PipeWireResult,
        SourceType,
        StreamConfig,
        StreamHandle,
        StreamInfo,
        VideoFrame,
    };
}

/// Video processing types (convenience re-export)
pub mod video {
    pub use lamco_video::{
        BitmapConverter, BitmapData, BitmapUpdate, ConversionError, DispatcherConfig,
        FrameDispatcher, FrameProcessor, ProcessorConfig, RdpPixelFormat, Rectangle,
    };
}

/// Input handling types (convenience re-export)
pub mod input {
    pub use lamco_rdp_input::{
        CoordinateTransformer, InputError, InputTranslator, KeyModifiers, KeyboardEvent,
        KeyboardEventType, KeyboardHandler, LinuxInputEvent, MonitorInfo, MouseButton, MouseEvent,
        MouseHandler, RdpInputEvent, Result as InputResult,
    };
}
