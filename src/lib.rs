//! Wayland RDP server — Portal mode for desktop sharing.
//!
//! **Video:** Portal → PipeWire → Display Handler → IronRDP → Client
//!
//! **Input:** Client → IronRDP → Input Handler → Portal → Compositor
//!
//! **Clipboard:** Client ↔ IronRDP ↔ Clipboard Orchestrator ↔ Portal ↔ Compositor

#![warn(clippy::all)]
// Test-only lint relaxations. Each of these is either meaningless or actively
// misleading in a test, and all of them stay in force for production code:
//
// - unwrap/expect: a panic IS the failure signal in a test. Rewriting every
//   assertion to propagate errors would obscure what is being asserted.
// - field_reassign_with_default: building a config by mutating a default is the
//   clearest way to show which single field a test case varies.
// - approx_constant: tests use values like 3.14 as arbitrary sample data, not
//   as an approximation of PI.
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::field_reassign_with_default,
        clippy::approx_constant
    )
)]

pub mod audio;
pub mod capabilities;
pub mod clipboard;
pub mod compositor;
pub mod config;
pub mod cursor;
pub mod damage;
pub mod health;
pub mod multimon;
pub mod performance;
pub mod protocol;
pub mod rdp;
pub mod runtime;
pub mod security;
pub mod server;
pub mod services;
pub mod session;
pub mod third_party;
pub mod transport;

/// D-Bus management interface for GUI ↔ server communication.
/// Session bus for native/Flatpak, system bus for system services.
pub mod dbus;

/// EGFX graphics pipeline — H.264 video encoding over RDP Dynamic Virtual Channels.
/// Requires the `h264` feature.
pub mod egfx;

/// Mutter Direct D-Bus API — bypasses XDG Portal on GNOME 42+ (non-sandboxed only).
pub mod mutter;

/// GUI configuration interface (iced framework). Requires the `gui` feature.
#[cfg(feature = "gui")]
pub mod gui;

/// HTTP metrics server (Prometheus + /health). Requires the `metrics-server` feature.
#[cfg(feature = "metrics-server")]
pub mod metrics_server;

pub use lamco_clipboard_core;
pub use lamco_pipewire;
pub use lamco_portal;
pub use lamco_rdp_clipboard;
pub use lamco_rdp_input;
pub use lamco_video;

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
        MonitorInfo, MultiStreamConfig, MultiStreamCoordinator, PipeWireConfig,
        PipeWireConfigBuilder, PipeWireConnection, PipeWireError, PipeWireManager,
        PipeWireThreadCommand, PipeWireThreadManager, PixelFormat, Result as PipeWireResult,
        SourceType, StreamConfig, StreamHandle, StreamInfo, VideoFrame,
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
        MouseHandler, RdpInputEvent, Result as InputResult, TouchContactFlags, TouchEvent,
        TouchHandler,
    };
}
