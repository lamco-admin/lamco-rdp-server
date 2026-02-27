//! EGFX (Graphics Pipeline Extension) Configuration Tab
//!
//! H.264 encoding settings, AVC444 configuration, and quality parameters.

use iced::{
    widget::{button, column, pick_list, row, slider, text},
    Alignment, Element, Length,
};

use crate::gui::{
    message::{EgfxPreset, Message},
    state::AppState,
    theme, widgets,
    widgets::space,
};

const ZGFX_OPTIONS: &[&str] = &["never", "auto", "always"];
const CODEC_OPTIONS: &[&str] = &["auto", "avc420", "avc444"];
const COLOR_MATRIX_OPTIONS: &[&str] = &["auto", "openh264", "bt709", "bt601", "srgb"];
const COLOR_RANGE_OPTIONS: &[&str] = &["auto", "limited", "full"];

pub fn view_egfx_tab(state: &AppState) -> Element<'_, Message> {
    let egfx = &state.config.egfx;

    column![
        // Section header
        widgets::section_header("EGFX (Graphics Pipeline) Configuration"),
        space().height(16.0),
        // Enable EGFX toggle - pending: requires RemoteFX fallback implementation
        widgets::toggle_switch(
            "Enable EGFX Graphics Pipeline",
            egfx.enabled,
            Message::EgfxEnabledToggled,
        ),
        text("⚠ Disabling requires RemoteFX fallback (not yet implemented)")
            .size(11)
            .style(|_theme| text::Style {
                color: Some(theme::colors::WARNING.scale_alpha(0.7)),
            }),
        space().height(16.0),
        // Quality presets
        text("Quality Presets:").size(14),
        space().height(8.0),
        row![
            button(text("Speed"))
                .on_press(Message::EgfxPresetSelected(EgfxPreset::Speed))
                .padding([8, 16])
                .style(theme::preset_button_style(
                    state.active_preset.as_deref() == Some("egfx_speed")
                )),
            button(text("Balanced"))
                .on_press(Message::EgfxPresetSelected(EgfxPreset::Balanced))
                .padding([8, 16])
                .style(theme::preset_button_style(
                    state.active_preset.as_deref() == Some("egfx_balanced")
                )),
            button(text("Quality"))
                .on_press(Message::EgfxPresetSelected(EgfxPreset::Quality))
                .padding([8, 16])
                .style(theme::preset_button_style(
                    state.active_preset.as_deref() == Some("egfx_quality")
                )),
        ]
        .spacing(8),
        space().height(20.0),
        // Basic Settings section
        widgets::subsection_header("Basic Settings"),
        space().height(12.0),
        widgets::labeled_row_with_help(
            "H.264 Bitrate:",
            150.0,
            row![
                widgets::number_input(
                    &state.edit_strings.h264_bitrate,
                    "5000",
                    80.0,
                    Message::EgfxH264BitrateChanged,
                ),
                text(" kbps"),
            ]
            .align_y(Alignment::Center)
            .into(),
            "Main stream bitrate (3000-15000 recommended)",
        ),
        space().height(12.0),
        widgets::labeled_row_with_help(
            "Codec:",
            150.0,
            pick_list(CODEC_OPTIONS.to_vec(), Some(egfx.codec.as_str()), |s| {
                Message::EgfxCodecChanged(s.to_string())
            },)
            .width(Length::Fixed(200.0))
            .into(),
            "Auto = best available | AVC420 = 4:2:0 | AVC444 = 4:4:4 (best quality)",
        ),
        space().height(12.0),
        widgets::labeled_row_with_help(
            "Periodic Keyframe:",
            150.0,
            row![
                widgets::number_input(
                    &state.edit_strings.periodic_idr,
                    "5",
                    60.0,
                    Message::EgfxPeriodicIdrIntervalChanged,
                ),
                text(" seconds (0 = disabled)"),
            ]
            .align_y(Alignment::Center)
            .into(),
            "Force IDR frame periodically (clears artifacts)",
        ),
        space().height(20.0),
        // Quality Control section
        widgets::subsection_header("Quality Control (QP)"),
        space().height(12.0),
        widgets::labeled_row(
            "QP Min:",
            150.0,
            row![
                widgets::number_input(
                    &state.edit_strings.qp_min,
                    "10",
                    60.0,
                    Message::EgfxQpMinChanged,
                ),
                text(" (lower = better quality)"),
            ]
            .align_y(Alignment::Center)
            .into(),
        ),
        space().height(8.0),
        widgets::labeled_row(
            "QP Default:",
            150.0,
            widgets::number_input(
                &state.edit_strings.qp_default,
                "23",
                60.0,
                Message::EgfxQpDefaultChanged,
            ),
        ),
        space().height(8.0),
        widgets::labeled_row(
            "QP Max:",
            150.0,
            row![
                widgets::number_input(
                    &state.edit_strings.qp_max,
                    "40",
                    60.0,
                    Message::EgfxQpMaxChanged,
                ),
                text(" (higher = lower quality)"),
            ]
            .align_y(Alignment::Center)
            .into(),
        ),
        space().height(8.0),
        widgets::info_box("Lower QP = better quality, higher bitrate. Range: 0-51."),
        space().height(20.0),
        // Expert mode toggle
        button(text(if state.egfx_expert_mode {
            "Hide Expert Settings"
        } else {
            "Show Expert Settings"
        }))
        .on_press(Message::EgfxToggleExpertMode)
        .padding([8, 16])
        .style(theme::secondary_button_style),
        if state.egfx_expert_mode {
            view_egfx_expert_settings(state)
        } else {
            column![].into()
        },
    ]
    .spacing(4)
    .padding(20)
    .into()
}

fn view_egfx_expert_settings(state: &AppState) -> Element<'_, Message> {
    let egfx = &state.config.egfx;

    column![
        space().height(20.0),
        // Advanced EGFX
        widgets::subsection_header("Advanced EGFX"),
        space().height(12.0),
        // Note: H.264 Level removed - OpenH264 auto-selects based on resolution,
        // and codec selection (avc420/avc444) is handled by the Codec dropdown
        widgets::labeled_row_with_help(
            "ZGFX Compression:",
            150.0,
            pick_list(
                ZGFX_OPTIONS.to_vec(),
                Some(egfx.zgfx_compression.as_str()),
                |s| Message::EgfxZgfxCompressionChanged(s.to_string()),
            )
            .width(Length::Fixed(120.0))
            .into(),
            "RDP8 bulk compression for EGFX PDUs (never/auto/always)",
        ),
        space().height(8.0),
        widgets::labeled_row(
            "Max Frames in Flight:",
            150.0,
            widgets::number_input(
                &state.edit_strings.max_frames,
                "3",
                60.0,
                Message::EgfxMaxFramesInFlightChanged,
            ),
        ),
        space().height(8.0),
        widgets::labeled_row_pending_with_note(
            "Frame Ack Timeout:",
            150.0,
            row![
                widgets::number_input(
                    &state.edit_strings.frame_ack_timeout,
                    "5000",
                    80.0,
                    Message::EgfxFrameAckTimeoutChanged,
                ),
                text(" ms"),
            ]
            .align_y(Alignment::Center)
            .into(),
            "Per-frame ack tracking not yet implemented; max_frames_in_flight handles backpressure",
        ),
        space().height(20.0),
        // AVC444 Configuration
        widgets::subsection_header("AVC444 Configuration (4:4:4 Chroma)"),
        space().height(12.0),
        widgets::toggle_with_help(
            "Enable AVC444",
            egfx.avc444_enabled,
            "Superior text/UI rendering, requires modern client",
            Message::EgfxAvc444EnabledToggled,
        ),
        space().height(12.0),
        widgets::labeled_row_pending_with_note(
            "Aux Bitrate Ratio:",
            150.0,
            row![
                slider(
                    30..=100,
                    (egfx.avc444_aux_bitrate_ratio * 100.0) as u32,
                    |v| { Message::EgfxAvc444AuxBitrateRatioChanged(v as f32 / 100.0) }
                )
                .width(Length::Fixed(150.0)),
                space().width(10.0),
                text(format!(
                    "{}%",
                    (egfx.avc444_aux_bitrate_ratio * 100.0) as u32
                )),
            ]
            .align_y(Alignment::Center)
            .into(),
            "Single encoder per MS-RDPEGFX spec",
        ),
        space().height(12.0),
        widgets::labeled_row(
            "Color Matrix:",
            150.0,
            pick_list(
                COLOR_MATRIX_OPTIONS.to_vec(),
                Some(egfx.color_matrix.as_str()),
                |s| Message::EgfxColorMatrixChanged(s.to_string()),
            )
            .width(Length::Fixed(150.0))
            .into(),
        ),
        space().height(8.0),
        widgets::labeled_row(
            "Color Range:",
            150.0,
            pick_list(
                COLOR_RANGE_OPTIONS.to_vec(),
                Some(egfx.color_range.as_str()),
                |s| Message::EgfxColorRangeChanged(s.to_string()),
            )
            .width(Length::Fixed(150.0))
            .into(),
        ),
        space().height(20.0),
        // AVC444 Aux Omission
        widgets::subsection_header("AVC444 Aux Omission (Bandwidth Optimization)"),
        space().height(12.0),
        widgets::toggle_with_help(
            "Enable Auxiliary Stream Omission",
            egfx.avc444_enable_aux_omission,
            "Skip aux when unchanged (FreeRDP-compatible, saves bandwidth)",
            Message::EgfxAvc444EnableAuxOmissionToggled,
        ),
        space().height(12.0),
        widgets::labeled_row(
            "Max Aux Interval:",
            150.0,
            row![
                widgets::number_input(
                    &state.edit_strings.max_aux_interval,
                    "30",
                    60.0,
                    Message::EgfxAvc444MaxAuxIntervalChanged,
                ),
                text(" frames"),
            ]
            .align_y(Alignment::Center)
            .into(),
        ),
        space().height(8.0),
        widgets::labeled_row(
            "Aux Change Threshold:",
            150.0,
            row![
                widgets::float_slider(
                    egfx.avc444_aux_change_threshold,
                    Message::EgfxAvc444AuxChangeThresholdChanged,
                ),
                text(format!(
                    " ({}%)",
                    (egfx.avc444_aux_change_threshold * 100.0) as u32
                )),
            ]
            .align_y(Alignment::Center)
            .into(),
        ),
        space().height(12.0),
        widgets::toggle_switch(
            "Force Aux IDR on Return",
            egfx.avc444_force_aux_idr_on_return,
            Message::EgfxAvc444ForceAuxIdrToggled,
        ),
        space().height(4.0),
        widgets::warning_box("Must be OFF for single encoder to allow Main P-frames (PRODUCTION)"),
    ]
    .into()
}
