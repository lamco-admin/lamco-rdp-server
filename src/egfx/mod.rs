//! EGFX (RDP Graphics Pipeline Extension) Integration
//!
//! This module integrates H.264 video streaming via the EGFX channel, using:
//! - `ironrdp-egfx` for protocol handling and frame transmission
//! - OpenH264 for actual video encoding
//! - PipeWire for screen capture
//!
//! # Architecture
//!
//! ```text
//! PipeWire → VideoFrame → EgfxVideoHandler → Avc420/444Encoder → H.264 NAL data
//!                              │                                       │
//!                              └───────────────────────────────────────┘
//!                                              │
//!                                              ▼
//!                                    LamcoGraphicsHandler
//!                                    (capability negotiation + callbacks)
//!                                              │
//!                                              │ send_avc420/444_frame()
//!                                              ▼
//!                                    EGFX Protocol Layer (internal)
//!                                              │
//!                                              │ DVC messages
//!                                              ▼
//!                                         RDP Client
//! ```
//!
//! # Codec Support
//!
//! - **AVC420**: Standard H.264 with 4:2:0 chroma subsampling (default)
//! - **AVC444**: Premium H.264 with 4:4:4 chroma via dual-stream encoding
//!
//! # API Boundaries
//!
//! This module exports only our own types. IronRDP types are used internally
//! by the server infrastructure but are not part of the public API.
//!
//! # Protocol Reference
//!
//! - [MS-RDPEGFX](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpegfx/)

// Core AVC420 encoder
mod encoder;

// AVC444 support (premium 4:4:4 chroma)
mod avc444_encoder;
mod color_convert;
mod color_space;
mod yuv444_packing;

// Hardware-accelerated encoding (VA-API, NVENC)
#[cfg(any(feature = "vaapi", feature = "nvenc"))]
pub mod hardware;

mod h264_level;
mod handler;
mod video_handler;

// Re-export our encoder types (clean API - no IronRDP types)
pub use encoder::{
    align_to_16, annex_b_to_avc, Avc420Encoder, EncoderConfig, EncoderError, EncoderResult,
    EncoderStats, H264Frame,
};

// Re-export AVC444 encoder types
pub use avc444_encoder::{Avc444Encoder, Avc444Frame, Avc444Stats, Avc444Timing};

// Re-export color conversion types (useful for custom processing)
pub use color_convert::{bgra_to_yuv444, subsample_chroma_420, ColorMatrix, Yuv444Frame};

// Re-export comprehensive color space configuration
pub use color_space::{
    ColorRange, ColorSpaceConfig, ColourPrimaries, MatrixCoefficients, TransferCharacteristics,
};

// Re-export YUV444 packing types
pub use yuv444_packing::{
    pack_auxiliary_view, pack_dual_views, pack_main_view, validate_dimensions, Yuv420Frame,
};

// Re-export H.264 level management
pub use h264_level::{ConstraintViolation, H264Level, LevelConstraints};

// Re-export our handler implementation
// Note: LamcoGraphicsHandler implements ironrdp_egfx::GraphicsPipelineHandler internally
// but that trait is not part of our public API
pub use handler::{LamcoGraphicsHandler, SharedGraphicsHandler};

// Re-export video handler types (clean API - no IronRDP types)
pub use video_handler::{EgfxVideoConfig, EgfxVideoHandler, EncodedFrame, EncodingStats};

// Re-export hardware encoder types (when feature enabled)
#[cfg(any(feature = "vaapi", feature = "nvenc"))]
pub use hardware::{
    create_hardware_encoder, HardwareEncoder, HardwareEncoderError, HardwareEncoderResult,
    HardwareEncoderStats, QualityPreset,
};

// Note: IronRDP EGFX types (Avc420Region, GraphicsPipelineServer, etc.) are NOT
// re-exported here. They are used internally by src/server/gfx_factory.rs which
// bridges our implementation with IronRDP's infrastructure.
