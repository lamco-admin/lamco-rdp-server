//! Multi-Monitor Support Module
//!
//! Provides comprehensive multi-monitor support for RDP sessions with production-grade
//! layout calculation, coordinate transformations, and dynamic reconfiguration.
//!
//! # Overview
//!
//! This module enables RDP clients to connect to Wayland sessions with multiple monitors,
//! presenting them as a unified virtual desktop with correct layout and coordinate mapping.
//!
//! # Features
//!
//! - **Monitor Discovery:** Enumerate monitors from Portal StreamInfo
//! - **Layout Calculation:** Compute virtual desktop layout with multiple strategies
//! - **Coordinate Transformation:** Map between RDP client space and monitor-local space
//! - **Dynamic Reconfiguration:** Handle monitor hotplug and resolution changes
//! - **IronRDP Integration:** DisplayControl protocol for client-side monitor management
//!
//! # Layout Strategies
//!
//! The module supports multiple layout strategies for monitor arrangement:
//!
//! ## 1. PreservePositions (Default)
//!
//! Maintains the exact monitor positions reported by the Wayland compositor:
//!
//! ```text
//! ┌────────┐     ┌────────┐
//! │Monitor1│     │Monitor2│
//! │1920x1080    │1920x1080
//! │(0, 0)  │     │(1920,0)│
//! └────────┘     └────────┘
//! ```
//!
//! ## 2. Horizontal
//!
//! Arranges monitors left-to-right regardless of actual positions:
//!
//! ```text
//! ┌────────┬────────┬────────┐
//! │Monitor1│Monitor2│Monitor3│
//! └────────┴────────┴────────┘
//! ```
//!
//! ## 3. Vertical
//!
//! Arranges monitors top-to-bottom:
//!
//! ```text
//! ┌────────┐
//! │Monitor1│
//! ├────────┤
//! │Monitor2│
//! ├────────┤
//! │Monitor3│
//! └────────┘
//! ```
//!
//! ## 4. Grid
//!
//! Arranges monitors in a rows × columns grid:
//!
//! ```text
//! ┌────────┬────────┐
//! │Monitor1│Monitor2│
//! ├────────┼────────┤
//! │Monitor3│Monitor4│
//! └────────┴────────┘
//! ```
//!
//! # Virtual Desktop
//!
//! All monitor layouts produce a **VirtualDesktop** that represents the combined
//! coordinate space:
//!
//! - **width/height:** Bounding box dimensions
//! - **offset_x/offset_y:** Top-left corner position (may be negative)
//! - **monitors:** Array of MonitorLayout with positions
//!
//! # Coordinate Transformation
//!
//! The module provides bidirectional coordinate mapping:
//!
//! - **RDP → Monitor:** Map client coordinates to monitor-local coordinates
//! - **Monitor → RDP:** Map monitor-local to client coordinates
//!
//! This is essential for:
//! - Mouse input (RDP coords → Wayland coords)
//! - Touch events
//! - Window positioning
//!
//! # Example
//!
//! ```no_run
//! use wrd_server::multimon::{MonitorManager, MultiMonitorConfig};
//! use wrd_server::portal::session::StreamInfo;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! // Create monitor manager
//! let config = MultiMonitorConfig::default();
//! let manager = MonitorManager::new(config);
//!
//! // Initialize from Portal streams
//! let streams: Vec<StreamInfo> = vec![/* ... */];
//! manager.initialize_from_streams(&streams).await?;
//!
//! // Get calculated layout
//! if let Some(layout) = manager.get_layout().await {
//!     println!("Virtual desktop: {}x{}",
//!         layout.virtual_desktop.width,
//!         layout.virtual_desktop.height
//!     );
//!
//!     // Transform RDP coordinates to monitor-local
//!     if let Some((monitor_id, local_x, local_y)) =
//!         layout.transform_rdp_to_monitor(1000, 500) {
//!         println!("Point at monitor {} ({}, {})", monitor_id, local_x, local_y);
//!     }
//! }
//! # Ok(())
//! # }
//! ```
//!
//! # Performance
//!
//! - **Latency:** <1ms for coordinate transformations
//! - **Memory:** O(n) where n = number of monitors
//! - **CPU:** Negligible overhead
//!
//! Implements TASK-P1-09 specification for production-grade multi-monitor handling.

mod layout;
mod manager;

pub use layout::{CoordinateSpace, Layout, LayoutCalculator, MonitorLayout, VirtualDesktop};
pub use manager::{MonitorEvent, MonitorInfo, MonitorManager, MultiMonitorConfig};

use crate::multimon::layout::LayoutError;
use thiserror::Error;

/// Multi-monitor result type
pub type Result<T> = std::result::Result<T, MultiMonitorError>;

/// Multi-monitor error types
#[derive(Error, Debug)]
pub enum MultiMonitorError {
    /// Layout calculation failed
    #[error("Layout calculation failed: {0}")]
    LayoutCalculation(String),

    /// Monitor not found
    #[error("Monitor not found: {0}")]
    MonitorNotFound(u32),

    /// Invalid configuration
    #[error("Invalid configuration: {0}")]
    InvalidConfiguration(String),

    /// Coordinate transformation error
    #[error("Coordinate transformation error: {0}")]
    CoordinateTransformation(String),

    /// Portal error
    #[error("Portal error: {0}")]
    Portal(String),

    /// Layout error
    #[error("Layout error: {0}")]
    Layout(#[from] LayoutError),

    /// IO error
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}
