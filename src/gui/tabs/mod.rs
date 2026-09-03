//! Tab layout functions for each configuration category.
//!
//! Tabs use shared widgets/theme for consistency; each tab owns
//! only the domain-specific arrangement of controls.
//!
//! Note: Multimon and per-output display settings live in the Display tab.
//! Note: Damage tracking lives in the Performance tab.
//! Note: Hardware encoding lives in the EGFX tab's expert settings.
//! Note: Cursor (including the predictor) lives in the Input tab.
//! Note: Logging settings remain in Advanced -> Logging & Diagnostics,
//! alongside video pipeline and advanced-video tuning.

mod advanced;
mod audio;
mod clipboard;
mod display;
mod egfx;
mod input;
mod performance;
mod security;
mod server;
mod status;
mod video;

pub use advanced::*;
pub use audio::*;
pub use clipboard::*;
pub use display::*;
pub use egfx::*;
pub use input::*;
pub use performance::*;
pub use security::*;
pub use server::*;
pub use status::*;
pub use video::*;
