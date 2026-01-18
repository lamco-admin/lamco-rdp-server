//! User-Friendly Error Formatting
//!
//! Provides user-friendly error messages with troubleshooting hints
//! for common error scenarios.

use std::fmt::Write;

/// Format error for user consumption
///
/// Takes technical error and produces user-friendly message with
/// troubleshooting steps and context.
pub fn format_user_error(error: &anyhow::Error) -> String {
    let mut output = String::new();

    // Header
    writeln!(&mut output, "").ok();
    writeln!(
        &mut output,
        "╔════════════════════════════════════════════════════════════╗"
    )
    .ok();
    writeln!(
        &mut output,
        "║                     ERROR                                  ║"
    )
    .ok();
    writeln!(
        &mut output,
        "╚════════════════════════════════════════════════════════════╝"
    )
    .ok();
    writeln!(&mut output, "").ok();

    // Analyze error and provide context
    let error_msg = error.to_string();

    if error_msg.contains("Portal") || error_msg.contains("portal") {
        format_portal_error(&mut output, &error_msg);
    } else if error_msg.contains("PipeWire") || error_msg.contains("pipewire") {
        format_pipewire_error(&mut output, &error_msg);
    } else if error_msg.contains("TLS") || error_msg.contains("certificate") {
        format_tls_error(&mut output, &error_msg);
    } else if error_msg.contains("bind") || error_msg.contains("address") {
        format_network_error(&mut output, &error_msg);
    } else if error_msg.contains("config") {
        format_config_error(&mut output, &error_msg);
    } else {
        format_generic_error(&mut output, &error_msg);
    }

    // Technical details
    writeln!(&mut output, "").ok();
    writeln!(
        &mut output,
        "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    )
    .ok();
    writeln!(&mut output, "Technical Details:").ok();
    writeln!(&mut output, "").ok();
    writeln!(&mut output, "{:#}", error).ok();
    writeln!(&mut output, "").ok();

    // Footer with help
    writeln!(
        &mut output,
        "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    )
    .ok();
    writeln!(&mut output, "Need Help?").ok();
    writeln!(
        &mut output,
        "  - Run with --verbose for detailed logs: lamco-rdp-server -vvv"
    )
    .ok();
    writeln!(&mut output, "  - Check logs in: /var/log/lamco-rdp-server/").ok();
    writeln!(
        &mut output,
        "  - Report issues: https://github.com/lamco-admin/wayland-rdp/issues"
    )
    .ok();
    writeln!(
        &mut output,
        "╚════════════════════════════════════════════════════════════╝"
    )
    .ok();

    output
}

fn format_portal_error(output: &mut String, _error: &str) {
    writeln!(output, "Screen Capture Permission Error").ok();
    writeln!(output, "").ok();
    writeln!(
        output,
        "Could not access the screen capture system (xdg-desktop-portal)."
    )
    .ok();
    writeln!(output, "").ok();
    writeln!(output, "Common Causes:").ok();
    writeln!(output, "").ok();
    writeln!(output, "  1. Portal permission denied").ok();
    writeln!(
        output,
        "     → When dialog appears, click 'Allow' or 'Share'"
    )
    .ok();
    writeln!(output, "     → Run the server again if you clicked 'Deny'").ok();
    writeln!(output, "").ok();
    writeln!(output, "  2. Portal is not running").ok();
    writeln!(
        output,
        "     → Run: systemctl --user status xdg-desktop-portal"
    )
    .ok();
    writeln!(
        output,
        "     → If not running: systemctl --user start xdg-desktop-portal"
    )
    .ok();
    writeln!(output, "").ok();
    writeln!(output, "  3. Portal backend not installed").ok();
    writeln!(
        output,
        "     → For GNOME: sudo apt install xdg-desktop-portal-gnome"
    )
    .ok();
    writeln!(
        output,
        "     → For KDE: sudo apt install xdg-desktop-portal-kde"
    )
    .ok();
    writeln!(
        output,
        "     → For wlroots: sudo apt install xdg-desktop-portal-wlr"
    )
    .ok();
    writeln!(output, "").ok();
    writeln!(output, "  4. Not running in Wayland session").ok();
    writeln!(
        output,
        "     → Check: echo $WAYLAND_DISPLAY (should not be empty)"
    )
    .ok();
    writeln!(
        output,
        "     → Log out and select 'Wayland' session at login"
    )
    .ok();
}

fn format_pipewire_error(output: &mut String, _error: &str) {
    writeln!(output, "Screen Capture System Error (PipeWire)").ok();
    writeln!(output, "").ok();
    writeln!(output, "Could not connect to PipeWire for video capture.").ok();
    writeln!(output, "").ok();
    writeln!(output, "Common Causes:").ok();
    writeln!(output, "").ok();
    writeln!(output, "  1. PipeWire is not running").ok();
    writeln!(output, "     → Run: systemctl --user status pipewire").ok();
    writeln!(
        output,
        "     → If not running: systemctl --user start pipewire"
    )
    .ok();
    writeln!(
        output,
        "     → Also start: systemctl --user start wireplumber"
    )
    .ok();
    writeln!(output, "").ok();
    writeln!(output, "  2. PipeWire version too old").ok();
    writeln!(output, "     → Run: pipewire --version").ok();
    writeln!(output, "     → Need: PipeWire 0.3.77 or newer").ok();
    writeln!(
        output,
        "     → Upgrade: sudo apt update && sudo apt upgrade pipewire"
    )
    .ok();
    writeln!(output, "").ok();
    writeln!(output, "  3. No permission to access video").ok();
    writeln!(
        output,
        "     → Add your user to video group: sudo usermod -aG video $USER"
    )
    .ok();
    writeln!(output, "     → Log out and log back in").ok();
}

fn format_tls_error(output: &mut String, _error: &str) {
    writeln!(output, "TLS Certificate Error").ok();
    writeln!(output, "").ok();
    writeln!(
        output,
        "Could not load TLS certificates for secure connections."
    )
    .ok();
    writeln!(output, "").ok();
    writeln!(output, "Common Causes:").ok();
    writeln!(output, "").ok();
    writeln!(output, "  1. Certificate files not found").ok();
    writeln!(output, "     → Check paths in config.toml").ok();
    writeln!(output, "     → Default: certs/cert.pem and certs/key.pem").ok();
    writeln!(output, "").ok();
    writeln!(output, "  2. Need to generate certificates").ok();
    writeln!(output, "     → Run: ./scripts/generate-certs.sh").ok();
    writeln!(output, "     → Or manually:").ok();
    writeln!(
        output,
        "       openssl req -x509 -newkey rsa:4096 -nodes \\"
    )
    .ok();
    writeln!(
        output,
        "         -keyout certs/key.pem -out certs/cert.pem \\"
    )
    .ok();
    writeln!(output, "         -days 365 -subj '/CN=lamco-rdp-server'").ok();
    writeln!(output, "").ok();
    writeln!(output, "  3. Invalid certificate format").ok();
    writeln!(output, "     → Certificates must be PEM format").ok();
    writeln!(
        output,
        "     → Check file starts with '-----BEGIN CERTIFICATE-----'"
    )
    .ok();
}

fn format_network_error(output: &mut String, _error: &str) {
    writeln!(output, "Network Binding Error").ok();
    writeln!(output, "").ok();
    writeln!(
        output,
        "Could not bind to network address for RDP connections."
    )
    .ok();
    writeln!(output, "").ok();
    writeln!(output, "Common Causes:").ok();
    writeln!(output, "").ok();
    writeln!(output, "  1. Port 3389 already in use").ok();
    writeln!(output, "     → Check: sudo ss -tlnp | grep 3389").ok();
    writeln!(output, "     → Kill other process or use different port").ok();
    writeln!(
        output,
        "     → Change in config.toml: listen_addr = '0.0.0.0:3390'"
    )
    .ok();
    writeln!(output, "").ok();
    writeln!(output, "  2. Permission denied (port < 1024)").ok();
    writeln!(output, "     → Use port >= 1024").ok();
    writeln!(output, "     → Or run with sudo (not recommended)").ok();
    writeln!(output, "").ok();
    writeln!(output, "  3. Invalid listen address").ok();
    writeln!(output, "     → Check config.toml: listen_addr format").ok();
    writeln!(output, "     → Should be: 'IP:PORT' like '0.0.0.0:3389'").ok();
}

fn format_config_error(output: &mut String, _error: &str) {
    writeln!(output, "Configuration Error").ok();
    writeln!(output, "").ok();
    writeln!(output, "Problem with configuration file.").ok();
    writeln!(output, "").ok();
    writeln!(output, "Common Causes:").ok();
    writeln!(output, "").ok();
    writeln!(output, "  1. Configuration file not found").ok();
    writeln!(
        output,
        "     → Default location: /etc/lamco-rdp-server/config.toml"
    )
    .ok();
    writeln!(
        output,
        "     → Or specify: lamco-rdp-server -c /path/to/config.toml"
    )
    .ok();
    writeln!(
        output,
        "     → Create from example: cp config.toml.example config.toml"
    )
    .ok();
    writeln!(output, "").ok();
    writeln!(output, "  2. Invalid TOML syntax").ok();
    writeln!(output, "     → Check for typos, missing quotes, etc.").ok();
    writeln!(output, "     → Validate with: toml-cli config.toml").ok();
    writeln!(output, "").ok();
    writeln!(output, "  3. Missing required fields").ok();
    writeln!(output, "     → Ensure all required sections present").ok();
    writeln!(output, "     → See config.toml.example for reference").ok();
}

fn format_generic_error(output: &mut String, error: &str) {
    writeln!(output, "Server Error").ok();
    writeln!(output, "").ok();
    writeln!(output, "An error occurred while running the server.").ok();
    writeln!(output, "").ok();
    writeln!(output, "Error: {}", error).ok();
    writeln!(output, "").ok();
    writeln!(output, "Troubleshooting:").ok();
    writeln!(output, "").ok();
    writeln!(output, "  1. Check all services are running:").ok();
    writeln!(output, "     → systemctl --user status pipewire").ok();
    writeln!(output, "     → systemctl --user status xdg-desktop-portal").ok();
    writeln!(output, "").ok();
    writeln!(output, "  2. Verify you're in a Wayland session:").ok();
    writeln!(output, "     → echo $WAYLAND_DISPLAY").ok();
    writeln!(
        output,
        "     → echo $XDG_SESSION_TYPE (should be 'wayland')"
    )
    .ok();
    writeln!(output, "").ok();
    writeln!(output, "  3. Check system requirements:").ok();
    writeln!(output, "     → GNOME 45+, KDE Plasma 6+, or Sway 1.8+").ok();
    writeln!(output, "     → PipeWire 0.3.77+").ok();
    writeln!(output, "     → xdg-desktop-portal 1.18+").ok();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_user_error() {
        let error = anyhow::anyhow!("Portal session creation failed");
        let formatted = format_user_error(&error);
        assert!(formatted.contains("ERROR"));
        assert!(formatted.contains("Portal"));
    }

    #[test]
    fn test_pipewire_error_formatting() {
        let error = anyhow::anyhow!("Failed to connect to PipeWire");
        let formatted = format_user_error(&error);
        assert!(formatted.contains("PipeWire"));
        assert!(formatted.contains("systemctl"));
    }
}
