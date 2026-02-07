//! Virtual Keyboard Implementation for zwp-virtual-keyboard Protocol
//!
//! This module provides a wrapper around `zwp_virtual_keyboard_v1` protocol,
//! handling keyboard input injection for Wayland compositors.
//!
//! # XKB Keymap Requirement
//!
//! The virtual keyboard protocol REQUIRES an XKB keymap to be provided before
//! any key events can be sent. This keymap defines the keyboard layout and
//! how keycodes map to keysyms.
//!
//! We generate the keymap using libxkbcommon from system defaults, respecting:
//! - $XKB_DEFAULT_RULES environment variable (or system default)
//! - $XKB_DEFAULT_MODEL environment variable (or system default)
//! - $XKB_DEFAULT_LAYOUT environment variable (or system default)
//! - $XKB_DEFAULT_VARIANT environment variable (or system default)
//! - $XKB_DEFAULT_OPTIONS environment variable (or system default)
//!
//! This ensures the virtual keyboard matches the user's actual keyboard configuration.
//!
//! # Keycode Format
//!
//! Key events use Linux evdev keycodes (not scancodes):
//! - KEY_A = 30
//! - KEY_B = 48
//! - KEY_ENTER = 28
//! - etc.
//!
//! The input handler has already translated RDP scancodes to evdev keycodes,
//! so we just forward them to the protocol.
//!
//! # Protocol Details
//!
//! The zwp_virtual_keyboard_v1 protocol is part of the standard
//! virtual-keyboard-unstable-v1 protocol (zwp namespace), supported by most
//! Wayland compositors including wlroots, GNOME (via RemoteDesktop portal),
//! and KDE (via RemoteDesktop portal).

use anyhow::{anyhow, Context, Result};
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::io::FromRawFd;
use tracing::{debug, info, warn};
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::QueueHandle;
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1,
    zwp_virtual_keyboard_v1::{self, ZwpVirtualKeyboardV1},
};
use xkbcommon::xkb;

/// Virtual keyboard wrapper for zwp-virtual-keyboard-v1 protocol
///
/// Wraps the Wayland protocol object and provides a clean Rust API for
///keyboard input injection. Handles XKB keymap generation and sharing.
///
/// # Lifecycle
///
/// The virtual keyboard is created once during session initialization:
/// 1. Generate XKB keymap from system defaults
/// 2. Create memfd with keymap content
/// 3. Send keymap to compositor via protocol
/// 4. Ready to send key events
///
/// The keyboard is reused for all key events and automatically destroyed when dropped.
pub struct VirtualKeyboard {
    keyboard: ZwpVirtualKeyboardV1,
    /// Keep the keymap fd alive for the lifetime of the keyboard
    /// The compositor may read from it at any time
    _keymap_fd: OwnedFd,
}

impl VirtualKeyboard {
    pub fn new<State>(
        manager: &ZwpVirtualKeyboardManagerV1,
        seat: &WlSeat,
        qh: &QueueHandle<State>,
        keyboard_layout: &str,
    ) -> Result<Self>
    where
        State: 'static,
    {
        info!(
            "🔑 wlr_direct: Creating virtual keyboard with XKB keymap (layout: {})",
            if keyboard_layout.is_empty() || keyboard_layout == "auto" {
                "system default"
            } else {
                keyboard_layout
            }
        );

        let keymap_string =
            generate_xkb_keymap(keyboard_layout).context("Failed to generate XKB keymap")?;

        debug!(
            "[wlr_direct] Generated XKB keymap: {} bytes",
            keymap_string.len()
        );

        let keymap_fd = create_keymap_fd(&keymap_string)
            .context("Failed to create shared memory fd for XKB keymap")?;

        debug!("[wlr_direct] Created memfd for keymap");

        let keyboard = manager.create_virtual_keyboard(seat, qh, ());

        // KeymapFormat::XkbV1 = 1 in the protocol
        keyboard.keymap(
            1u32, // XKB_V1 format
            keymap_fd.as_raw_fd(),
            keymap_string.len() as u32,
        );

        info!("✅ wlr_direct: Virtual keyboard created with system keymap");

        Ok(Self {
            keyboard,
            _keymap_fd: keymap_fd,
        })
    }

    pub fn key(&self, time: u32, keycode: u32, state: KeyState) {
        // KeyState in wayland-protocols uses u32:
        // 0 = released, 1 = pressed
        let state_val = match state {
            KeyState::Released => 0u32,
            KeyState::Pressed => 1u32,
        };

        debug!(
            "[wlr_direct] Keyboard key: keycode={}, state={:?}",
            keycode, state
        );

        self.keyboard.key(time, keycode, state_val);
    }

    /// Send modifier state (Ctrl, Alt, Shift, etc.)
    ///
    /// For basic operation, all zeros works -- the compositor tracks state from key events.
    pub fn modifiers(&self, depressed: u32, latched: u32, locked: u32, group: u32) {
        debug!(
            "[wlr_direct] Keyboard modifiers: depressed={:x}, latched={:x}, locked={:x}, group={}",
            depressed, latched, locked, group
        );

        self.keyboard.modifiers(depressed, latched, locked, group);
    }

    pub fn inner(&self) -> &ZwpVirtualKeyboardV1 {
        &self.keyboard
    }
}

impl Drop for VirtualKeyboard {
    fn drop(&mut self) {
        debug!("🔌 wlr_direct: Virtual keyboard destroyed");
        self.keyboard.destroy();
    }
}

/// Key state for keyboard events
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyState {
    Released,
    Pressed,
}

impl From<bool> for KeyState {
    fn from(pressed: bool) -> Self {
        if pressed {
            KeyState::Pressed
        } else {
            KeyState::Released
        }
    }
}

/// Generate XKB keymap with optional layout override.
///
/// Empty or "auto" layout uses system defaults from $XKB_DEFAULT_LAYOUT.
/// Keymap sources: $XKB_DEFAULT_RULES, $XKB_DEFAULT_MODEL, $XKB_DEFAULT_VARIANT, $XKB_DEFAULT_OPTIONS.
fn generate_xkb_keymap(layout: &str) -> Result<String> {
    let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);

    // Determine layout: "auto" or empty means use system default
    let layout_str = if layout.is_empty() || layout == "auto" {
        "" // Empty = use $XKB_DEFAULT_LAYOUT or "us"
    } else {
        layout
    };

    if !layout_str.is_empty() {
        debug!(
            "[wlr_direct] Using keyboard layout from config: {}",
            layout_str
        );
    } else {
        debug!("[wlr_direct] Using system default keyboard layout");
    }

    let keymap = xkb::Keymap::new_from_names(
        &context,
        "",         // rules: $XKB_DEFAULT_RULES or "evdev"
        "",         // model: $XKB_DEFAULT_MODEL or "pc105"
        layout_str, // layout: from config, or $XKB_DEFAULT_LAYOUT or "us"
        "",         // variant: $XKB_DEFAULT_VARIANT or ""
        None,       // options: $XKB_DEFAULT_OPTIONS or None
        xkb::KEYMAP_COMPILE_NO_FLAGS,
    )
    .ok_or_else(|| {
        anyhow!(
            "Failed to compile XKB keymap from system defaults. \
             Check XKB_DEFAULT_* environment variables or system XKB configuration."
        )
    })?;

    let keymap_string = keymap.get_as_string(xkb::KEYMAP_FORMAT_TEXT_V1);

    if keymap_string.is_empty() {
        return Err(anyhow!(
            "XKB keymap generation returned empty string. \
             This indicates a serious XKB configuration issue."
        ));
    }

    if !keymap_string.contains("xkb_keymap") {
        warn!(
            "⚠️  Generated XKB keymap may be malformed (missing 'xkb_keymap' marker). \
             Keyboard input may not work correctly."
        );
    }

    debug!(
        "[wlr_direct] XKB keymap preview: {} bytes, starts with: {}",
        keymap_string.len(),
        keymap_string.chars().take(80).collect::<String>().trim()
    );

    Ok(keymap_string)
}

/// Create a memfd for sharing the XKB keymap with the compositor.
///
/// Requires Linux 3.17+ (memfd_create syscall).
/// This is available on all modern distributions (Ubuntu 14.04+, RHEL 7+, etc.)
fn create_keymap_fd(keymap: &str) -> Result<OwnedFd> {
    use nix::sys::memfd::{memfd_create, MemFdCreateFlag};
    use nix::unistd::write;
    use std::ffi::CString;
    use std::os::fd::AsFd;

    let name = CString::new("xkb-keymap")?;
    let fd = memfd_create(
        &name,
        MemFdCreateFlag::MFD_CLOEXEC | MemFdCreateFlag::MFD_ALLOW_SEALING,
    )
    .context("Failed to create memfd. Requires Linux 3.17+ with memfd_create support.")?;

    let owned_fd = unsafe { OwnedFd::from_raw_fd(fd) };

    let bytes = keymap.as_bytes();
    let mut written = 0;

    while written < bytes.len() {
        match write(owned_fd.as_fd(), &bytes[written..]) {
            Ok(n) => {
                if n == 0 {
                    return Err(anyhow!(
                        "Failed to write keymap to memfd: unexpected zero-length write at offset {}",
                        written
                    ));
                }
                written += n;
            }
            Err(nix::errno::Errno::EINTR) => {
                // Interrupted by signal, retry
                continue;
            }
            Err(e) => {
                return Err(anyhow!(
                    "Failed to write keymap to memfd at offset {}: {}",
                    written,
                    e
                ));
            }
        }
    }

    debug!(
        "[wlr_direct] Wrote {} bytes to memfd for XKB keymap",
        written
    );

    // Note: nix crate doesn't provide fcntl_add_seals, but memfd sealing is optional
    // The keymap fd will work without sealing, just less secure

    use nix::unistd::{lseek, Whence};
    lseek(owned_fd.as_fd().as_raw_fd(), 0, Whence::SeekSet)
        .context("Failed to seek memfd to beginning")?;

    Ok(owned_fd)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_key_state_from_bool() {
        assert_eq!(KeyState::from(true), KeyState::Pressed);
        assert_eq!(KeyState::from(false), KeyState::Released);
    }

    #[test]
    fn test_generate_xkb_keymap() {
        // Test that we can generate a keymap with system default layout
        // This requires XKB to be installed on the system
        match generate_xkb_keymap("auto") {
            Ok(keymap) => {
                assert!(!keymap.is_empty());
                assert!(keymap.contains("xkb_keymap"));
                println!("Generated keymap (auto): {} bytes", keymap.len());
            }
            Err(e) => {
                // This may fail in minimal test environments without XKB installed
                println!(
                    "XKB keymap generation failed (expected in some test envs): {}",
                    e
                );
            }
        }

        // Test with explicit "us" layout
        match generate_xkb_keymap("us") {
            Ok(keymap) => {
                assert!(!keymap.is_empty());
                assert!(keymap.contains("xkb_keymap"));
                println!("Generated keymap (us): {} bytes", keymap.len());
            }
            Err(e) => {
                println!(
                    "XKB keymap generation for 'us' failed (expected in some test envs): {}",
                    e
                );
            }
        }
    }

    #[test]
    fn test_create_keymap_fd() {
        let test_keymap = "xkb_keymap { /* test keymap */ }";

        match create_keymap_fd(test_keymap) {
            Ok(fd) => {
                // Verify we can read back the data
                use nix::unistd::{lseek, read, Whence};
                use std::os::fd::AsFd;
                let mut buffer = vec![0u8; test_keymap.len()];

                // Seek to start before reading
                lseek(fd.as_fd().as_raw_fd(), 0, Whence::SeekSet).unwrap();

                let n = read(fd.as_raw_fd(), &mut buffer).unwrap();
                assert_eq!(n, test_keymap.len());
                assert_eq!(&buffer[..n], test_keymap.as_bytes());
            }
            Err(e) => {
                // This may fail on very old kernels (< 3.17)
                println!("memfd creation failed (expected on old kernels): {}", e);
            }
        }
    }

    #[test]
    fn test_keymap_fd_size() {
        let large_keymap = "x".repeat(100000); // 100KB keymap

        match create_keymap_fd(&large_keymap) {
            Ok(fd) => {
                // Verify size using fstat
                use nix::sys::stat::fstat;
                let metadata = fstat(fd.as_raw_fd()).unwrap();
                assert_eq!(metadata.st_size as usize, large_keymap.len());
            }
            Err(e) => {
                println!("memfd creation failed: {}", e);
            }
        }
    }
}
