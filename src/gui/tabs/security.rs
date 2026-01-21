//! Security Configuration Tab
//!
//! TLS certificates, authentication, NLA settings.

use iced::widget::{button, column, container, pick_list, row, space, text, text_input};
use iced::{Element, Length};

use crate::gui::message::Message;
use crate::gui::state::AppState;
use crate::gui::theme;
use crate::gui::widgets;

const AUTH_METHODS: &[&str] = &["pam", "none"];

pub fn view_security_tab(state: &AppState) -> Element<'_, Message> {
    let main_content = column![
        // Section header
        widgets::section_header("Security Configuration"),
        space().height(20.0),
        // TLS Certificate section
        text("TLS Certificate:").size(14),
        space().height(4.0),
        widgets::path_input(
            &state.edit_strings.cert_path,
            "/path/to/cert.pem",
            Message::SecurityCertPathChanged,
            Message::SecurityBrowseCert,
        ),
        space().height(8.0),
        // Generate certificate button
        button(text("Generate Self-Signed Certificate"))
            .on_press(Message::SecurityGenerateCert)
            .padding([8, 16])
            .style(theme::secondary_button_style),
        space().height(16.0),
        // TLS Private Key section
        text("TLS Private Key:").size(14),
        space().height(4.0),
        widgets::path_input(
            &state.edit_strings.key_path,
            "/path/to/key.pem",
            Message::SecurityKeyPathChanged,
            Message::SecurityBrowseKey,
        ),
        space().height(20.0),
        // Enable NLA
        widgets::toggle_with_help(
            "Enable Network Level Authentication (NLA)",
            state.config.security.enable_nla,
            "Requires client to authenticate before connection is established",
            Message::SecurityEnableNlaToggled,
        ),
        space().height(16.0),
        // Authentication Method
        widgets::labeled_row_with_help(
            "Authentication Method:",
            150.0,
            pick_list(
                AUTH_METHODS.to_vec(),
                Some(state.config.security.auth_method.as_str()),
                |s| Message::SecurityAuthMethodChanged(s.to_string()),
            )
            .width(Length::Fixed(150.0))
            .into(),
            "PAM = system authentication, None = no password required",
        ),
        space().height(16.0),
        // Require TLS 1.3
        widgets::toggle_with_help(
            "Require TLS 1.3 or higher",
            state.config.security.require_tls_13,
            "Recommended for security, may block older clients",
            Message::SecurityRequireTls13Toggled,
        ),
    ]
    .spacing(4)
    .padding(20);

    // Certificate generation dialog overlay
    if let Some(ref cert_state) = state.cert_gen_dialog {
        let dialog = view_cert_gen_dialog(cert_state);
        // In a real implementation, this would be a modal overlay
        column![main_content, space().height(20.0), dialog].into()
    } else {
        main_content.into()
    }
}

fn view_cert_gen_dialog(cert_state: &crate::gui::state::CertGenState) -> Element<'_, Message> {
    container(
        column![
            text("Generate Self-Signed Certificate").size(18),
            space().height(16.0),
            widgets::labeled_row(
                "Common Name:",
                120.0,
                text_input("localhost", &cert_state.common_name)
                    .on_input(Message::CertGenCommonNameChanged)
                    .width(Length::Fixed(250.0))
                    .style(theme::text_input_style)
                    .into(),
            ),
            space().height(8.0),
            widgets::labeled_row(
                "Organization:",
                120.0,
                text_input("My Organization", &cert_state.organization)
                    .on_input(Message::CertGenOrganizationChanged)
                    .width(Length::Fixed(250.0))
                    .style(theme::text_input_style)
                    .into(),
            ),
            space().height(8.0),
            widgets::labeled_row(
                "Valid Days:",
                120.0,
                widgets::number_input(&cert_state.valid_days_str, "365", 100.0, |s| {
                    Message::CertGenValidDaysChanged(s)
                },),
            ),
            space().height(20.0),
            row![
                button(text("Cancel"))
                    .on_press(Message::CertGenCancel)
                    .padding([8, 16])
                    .style(theme::secondary_button_style),
                space().width(Length::Fill),
                button(text(if cert_state.generating {
                    "Generating..."
                } else {
                    "Generate"
                }))
                .on_press_maybe(if cert_state.generating {
                    None
                } else {
                    Some(Message::CertGenConfirm)
                })
                .padding([8, 16])
                .style(theme::primary_button_style),
            ]
            .spacing(10),
        ]
        .spacing(8)
        .padding(20)
        .width(Length::Fixed(450.0)),
    )
    .padding(2)
    .style(theme::section_container_style)
    .into()
}
