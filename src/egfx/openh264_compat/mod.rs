//! Version-aware OpenH264 FFI abstraction layer.
//!
//! This module replaces direct use of the `openh264` crate's encoder with
//! a version-dispatched wrapper that supports both ABI 7 (OpenH264 2.3.1-2.5.x)
//! and ABI 8 (2.6.0+) from a single binary.
//!
//! The `openh264` crate generates FFI bindings from a single header version
//! at compile time. When the runtime library differs, struct layout mismatches
//! cause memory corruption and hangs. This module detects the runtime version
//! via `WelsGetCodecVersion()` and uses the correct struct layouts.
//!
//! See `docs/decisions/OPENH264-ABI-COMPATIBILITY.md` for the full ADR.

pub(super) mod encoder_core;
pub(super) mod ffi_types;
pub(super) mod loader;

pub(super) use encoder_core::{EncoderConfig, VersionedEncoder, VuiConfig};
pub(super) use ffi_types::{EncodedFrameData, HIGH_COMPLEXITY};
pub(super) use loader::{load_openh264, AbiGeneration, OpenH264Api};
