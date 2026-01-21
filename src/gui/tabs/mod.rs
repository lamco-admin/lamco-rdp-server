//! Tab layout functions for each configuration category.
//!
//! Tabs use shared widgets/theme for consistency; each tab owns
//! only the domain-specific arrangement of controls.

mod advanced;
mod audio;
mod clipboard;
mod egfx;
mod input;
mod logging;
mod performance;
mod security;
mod server;
mod status;
mod video;

pub use advanced::*;
pub use audio::*;
pub use clipboard::*;
pub use egfx::*;
pub use input::*;
pub use logging::*;
pub use performance::*;
pub use security::*;
pub use server::*;
pub use status::*;
pub use video::*;
