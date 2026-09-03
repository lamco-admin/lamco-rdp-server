//! Video Configuration Tab
//!
//! Core video settings: FPS, cursor mode, and Wayland capture protocol.
//!
//! Note: Encoder and bitrate settings are in their dedicated tabs:
//! - Hardware encoding: EGFX -> Hardware Encoding (expert settings)
//! - Bitrate: EGFX -> H.264 Bitrate
//! - Damage tracking: Performance -> Damage Tracking
//! - Video pipeline: Advanced -> Video Pipeline

use iced::{
    Alignment, Element, Length,
    widget::{column, pick_list, row, slider, text},
};

use crate::gui::{message::Message, state::AppState, theme, widgets, widgets::space};

/// Cursor rendering modes
const CURSOR_MODES: &[&str] = &["metadata", "embedded", "hidden"];

/// Wayland capture protocol preference (portal-generic strategy only)
const CAPTURE_PROTOCOLS: &[&str] = &["auto", "ext", "wlr"];

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
        // Capture Protocol section
        widgets::subsection_header("Capture Protocol"),
        space().height(4.0),
        text("Only used by the portal-generic strategy (direct compositor access); has no effect when using XDG Desktop Portal capture.")
            .size(12)
            .style(|_theme| text::Style {
                color: Some(theme::colors::TEXT_MUTED),
            }),
        space().height(12.0),
        widgets::labeled_row_with_help(
            "Protocol:",
            150.0,
            pick_list(
                CAPTURE_PROTOCOLS.to_vec(),
                Some(state.config.capture.protocol.as_str()),
                |s| Message::CaptureProtocolChanged(s.to_string()),
            )
            .width(Length::Fixed(150.0))
            .into(),
            "Auto prefers ext-image-copy-capture-v1, falls back to wlr-screencopy",
        ),
        space().height(8.0),
        widgets::toggle_with_help(
            "Allow Fallback",
            state.config.capture.allow_fallback,
            "Try the alternative protocol if the preferred one is unavailable",
            Message::CaptureAllowFallbackToggled,
        ),
        space().height(8.0),
        widgets::labeled_row_with_help(
            "Handshake Timeout:",
            150.0,
            row![
                widgets::number_input(
                    &state.edit_strings.capture_handshake_timeout,
                    "5000",
                    80.0,
                    Message::CaptureHandshakeTimeoutChanged,
                ),
                text(" ms (0 = disabled)"),
            ]
            .align_y(Alignment::Center)
            .into(),
            "How long to wait for the compositor to respond to an ext-capture request",
        ),
        space().height(24.0),
        // Related Settings note
        widgets::help_text(
            "Related settings: EGFX -> Hardware Encoding, EGFX -> H.264 Bitrate, Performance -> Damage Tracking, Advanced -> Video Pipeline"
        ),
    ]
    .spacing(4)
    .padding(20)
    .into()
}
