//! Configuration GUI built on iced's Elm Architecture.
//!
//! Uses a single `AppState` that mirrors the TOML config structure,
//! with `EditStrings` for text inputs to avoid iced lifetime issues.
//! Tab-based layout groups related settings (Server, Security, Video, etc.).

pub mod app;
pub mod capabilities;
pub mod certificates;
pub mod dbus_client;
pub mod file_ops;
pub mod hardware;
pub mod message;
pub mod server_connection;
pub mod server_process;
pub mod state;
pub mod tabs;
pub mod theme;
pub mod validation;
pub mod widgets;
