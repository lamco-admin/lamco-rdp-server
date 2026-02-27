//! Security Configuration Tab
//!
//! TLS certificates, authentication, NLA settings.

use iced::{
    widget::{button, column, container, pick_list, row, text, text_input},
    Element, Length,
};

use crate::gui::{message::Message, state::AppState, theme, widgets, widgets::space};

/// Default authentication methods (used when capabilities not yet detected)
/// "none" is first as it's the default and always available
const DEFAULT_AUTH_METHODS: &[&str] = &["none", "pam"];

/// Get available authentication methods based on detected capabilities
///
/// Returns the list from service registry if capabilities detected,
/// otherwise falls back to the default list.
fn get_auth_methods(state: &AppState) -> Vec<&str> {
    if let Some(ref caps) = state.detected_capabilities {
        // Use dynamic list from service registry
        caps.available_auth_methods
            .iter()
            .map(|s| s.as_str())
            .collect()
    } else {
        // Fall back to defaults until capabilities detected
        DEFAULT_AUTH_METHODS.to_vec()
    }
}

/// Get context-sensitive help text for authentication method
///
/// Provides different help based on whether PAM is available
/// (detected via service registry).
fn auth_method_help_text(state: &AppState) -> &'static str {
    if let Some(ref caps) = state.detected_capabilities {
        if caps.available_auth_methods.contains(&"pam".to_string()) {
            "PAM = system authentication, None = no password required"
        } else {
            // PAM unavailable (likely Flatpak)
            "PAM unavailable in this deployment. None = no password required"
        }
    } else {
        // Capabilities not yet detected
        "PAM = system authentication, None = no password required"
    }
}

pub fn view_security_tab(state: &AppState) -> Element<'_, Message> {
    let in_flatpak = crate::config::is_flatpak();

    // Cert/key path widgets: read-only in Flatpak (portal-mediated selection only)
    let cert_path_widget: Element<'_, Message> = if in_flatpak {
        widgets::path_display(
            &state.edit_strings.cert_path,
            "Select certificate via Browse...",
            Message::SecurityBrowseCert,
        )
    } else {
        widgets::path_input(
            &state.edit_strings.cert_path,
            "/path/to/cert.pem",
            Message::SecurityCertPathChanged,
            Message::SecurityBrowseCert,
        )
    };

    let key_path_widget: Element<'_, Message> = if in_flatpak {
        widgets::path_display(
            &state.edit_strings.key_path,
            "Select private key via Browse...",
            Message::SecurityBrowseKey,
        )
    } else {
        widgets::path_input(
            &state.edit_strings.key_path,
            "/path/to/key.pem",
            Message::SecurityKeyPathChanged,
            Message::SecurityBrowseKey,
        )
    };

    let main_content = column![
        // Section header
        widgets::section_header("Security Configuration"),
        space().height(20.0),
        // TLS Certificate section
        text("TLS Certificate:").size(14),
        space().height(4.0),
        cert_path_widget,
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
        key_path_widget,
        space().height(20.0),
        // Enable NLA
        widgets::toggle_pending_with_note(
            "Enable Network Level Authentication (NLA)",
            state.config.security.enable_nla,
            Message::SecurityEnableNlaToggled,
            "IronRDP lacks CredSSP/NLA support",
        ),
        space().height(16.0),
        // Authentication Method
        widgets::labeled_row_with_help(
            "Authentication Method:",
            150.0,
            pick_list(
                get_auth_methods(state),
                Some(state.config.security.auth_method.as_str()),
                |s| Message::SecurityAuthMethodChanged(s.to_string()),
            )
            .width(Length::Fixed(150.0))
            .into(),
            auth_method_help_text(state),
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
