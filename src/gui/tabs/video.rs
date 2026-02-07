//! Video Configuration Tab
//!
//! Core video settings: FPS and cursor mode.
//!
//! Note: Encoder and bitrate settings are in their dedicated tabs:
//! - Hardware encoding: Advanced -> Hardware Encoding
//! - Bitrate: EGFX -> H.264 Bitrate
//! - Damage tracking: Advanced -> Damage Tracking
//! - Video pipeline: Advanced -> Video Pipeline

use iced::widget::{column, pick_list, row, slider, space, text};
use iced::{Alignment, Element, Length};

use crate::gui::message::Message;
use crate::gui::state::AppState;
use crate::gui::widgets;

/// Cursor rendering modes
const CURSOR_MODES: &[&str] = &["metadata", "embedded", "hidden"];

pub fn view_video_tab(state: &AppState) -> Element<'_, Message> {
    column![
        // Section header
        widgets::section_header("Video Configuration"),
        space().height(20.0),
        // Basic Settings section
        widgets::subsection_header("Basic Settings"),
        space().height(12.0),
        // Target FPS
        widgets::labeled_row_with_help(
            "Target FPS:",
            150.0,
            row![
                slider(
                    5..=60,
                    state.config.video.target_fps,
                    Message::VideoTargetFpsChanged
                )
                .width(Length::Fixed(200.0)),
                space().width(10.0),
                text(format!("{} fps", state.config.video.target_fps)),
            ]
            .align_y(Alignment::Center)
            .into(),
            "5 <-----------------------> 60",
        ),
        space().height(12.0),
        // Cursor Mode
        widgets::labeled_row_with_help(
            "Cursor Mode:",
            150.0,
            pick_list(
                CURSOR_MODES.to_vec(),
                Some(state.config.video.cursor_mode.as_str()),
                |s| Message::VideoCursorModeChanged(s.to_string()),
            )
            .width(Length::Fixed(150.0))
            .into(),
            "Metadata = client-side (lowest latency)",
        ),
        space().height(24.0),
        // Related Settings note
        widgets::help_text(
            "Related settings: Advanced -> Hardware Encoding, EGFX -> H.264 Bitrate, Advanced -> Damage Tracking, Advanced -> Video Pipeline"
        ),
    ]
    .spacing(4)
    .padding(20)
    .into()
}
