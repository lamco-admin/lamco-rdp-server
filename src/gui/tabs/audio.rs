//! Audio Configuration Tab
//!
//! RDPSND audio output settings including codec selection, sample rate, and quality.

use iced::widget::{column, pick_list, row, space, text};
use iced::{Alignment, Element, Length};

use crate::gui::message::Message;
use crate::gui::state::AppState;
use crate::gui::widgets;

/// Available audio codecs
const AUDIO_CODECS: &[&str] = &["auto", "opus", "pcm", "adpcm"];

/// Available sample rates
const SAMPLE_RATES: &[u32] = &[8000, 16000, 22050, 44100, 48000];

/// Available channel configurations
const CHANNELS: &[u8] = &[1, 2];

/// Available frame durations (ms)
const FRAME_DURATIONS: &[u32] = &[10, 20, 40, 60];

pub fn view_audio_tab(state: &AppState) -> Element<'_, Message> {
    column![
        // Section header
        widgets::section_header("Audio Configuration (RDPSND)"),
        space().height(20.0),
        // Enable audio toggle
        widgets::toggle_with_help(
            "Enable Audio Output",
            state.config.audio.enabled,
            "Stream desktop audio to RDP client",
            Message::AudioEnabledToggled,
        ),
        space().height(16.0),
        // Codec selection
        widgets::labeled_row_with_help(
            "Codec:",
            150.0,
            pick_list(
                AUDIO_CODECS.to_vec(),
                Some(state.config.audio.codec.as_str()),
                |s| Message::AudioCodecChanged(s.to_string()),
            )
            .width(Length::Fixed(150.0))
            .into(),
            "Audio compression format",
        ),
        space().height(4.0),
        widgets::info_box(
            "• auto: Client chooses best supported codec\n\
             • opus: High quality, low bandwidth (recommended)\n\
             • pcm: Uncompressed, highest quality\n\
             • adpcm: Legacy compatibility"
        ),
        space().height(16.0),
        // Sample rate
        widgets::labeled_row_with_help(
            "Sample Rate:",
            150.0,
            row![
                pick_list(
                    SAMPLE_RATES.to_vec(),
                    Some(state.config.audio.sample_rate),
                    Message::AudioSampleRateChanged,
                )
                .width(Length::Fixed(100.0)),
                text(" Hz"),
            ]
            .spacing(4)
            .align_y(Alignment::Center)
            .into(),
            "Audio sample rate (48000 Hz recommended)",
        ),
        space().height(16.0),
        // Channels
        widgets::labeled_row_with_help(
            "Channels:",
            150.0,
            pick_list(
                CHANNELS.to_vec(),
                Some(state.config.audio.channels),
                Message::AudioChannelsChanged,
            )
            .width(Length::Fixed(100.0))
            .into(),
            "1 = Mono, 2 = Stereo",
        ),
        space().height(16.0),
        // Frame duration
        widgets::labeled_row_with_help(
            "Frame Duration:",
            150.0,
            row![
                pick_list(
                    FRAME_DURATIONS.to_vec(),
                    Some(state.config.audio.frame_ms),
                    Message::AudioFrameMsChanged,
                )
                .width(Length::Fixed(80.0)),
                text(" ms"),
            ]
            .spacing(4)
            .align_y(Alignment::Center)
            .into(),
            "Lower = less latency, higher overhead",
        ),
        space().height(16.0),
        // OPUS bitrate (only relevant when codec is opus or auto)
        widgets::labeled_row_with_help(
            "OPUS Bitrate:",
            150.0,
            row![
                widgets::number_input(
                    &state.edit_strings.audio_opus_bitrate,
                    "64",
                    80.0,
                    Message::AudioOpusBitrateChanged,
                ),
                text(" kbps"),
            ]
            .spacing(4)
            .align_y(Alignment::Center)
            .into(),
            "OPUS encoder bitrate (32-256 kbps typical)",
        ),
        space().height(20.0),
        // Info section
        widgets::info_box(
            "Audio is streamed from the Linux desktop to the RDP client via RDPSND.\n\
             PipeWire is required for audio capture.\n\n\
             Recommended settings for best experience:\n\
             • Codec: auto or opus\n\
             • Sample Rate: 48000 Hz\n\
             • Channels: 2 (stereo)\n\
             • Frame Duration: 20 ms"
        ),
    ]
    .spacing(4)
    .padding(20)
    .into()
}
