//! Cross-platform sanitization utilities for clipboard data.
//!
//! This module provides functions to safely convert clipboard data between
//! Linux and Windows environments, handling:
//!
//! - Filename character restrictions
//! - Reserved filenames
//! - Text encoding and line endings
//! - File URI parsing
//!
//! # Example
//!
//! ```rust
//! use lamco_clipboard_core::sanitize::{sanitize_filename_for_windows, parse_file_uris};
//!
//! // Sanitize a Linux filename for Windows
//! let safe = sanitize_filename_for_windows("file:with:colons.txt");
//! assert_eq!(safe, "file_with_colons.txt");
//!
//! // Parse file URIs from clipboard
//! let uris = b"copy\nfile:///home/user/test.txt\n";
//! let paths = parse_file_uris(uris);
//! ```

use std::path::PathBuf;

// =============================================================================
// Windows Filename Sanitization
// =============================================================================

/// Characters that are invalid in Windows filenames.
const WINDOWS_INVALID_CHARS: &[char] = &['\\', '/', ':', '*', '?', '"', '<', '>', '|'];

/// Reserved filenames in Windows (case-insensitive).
/// These cannot be used as filenames, even with extensions.
const WINDOWS_RESERVED_NAMES: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9", "LPT1", "LPT2",
    "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Maximum filename length for Windows (without path).
const WINDOWS_MAX_FILENAME_LEN: usize = 255;

/// Sanitize a filename for use on Windows.
///
/// This function:
/// - Replaces invalid characters (`\ / : * ? " < > |`) with underscores
/// - Handles reserved names (CON, PRN, AUX, NUL, COM1-9, LPT1-9) by prefixing with `_`
/// - Removes trailing dots and spaces (Windows silently strips these)
/// - Truncates to 255 characters if necessary
/// - Handles empty filenames
///
/// # Arguments
///
/// * `filename` - The original filename (just the name, not a full path)
///
/// # Returns
///
/// A sanitized filename safe for use on Windows filesystems.
///
/// # Example
///
/// ```rust
/// use lamco_clipboard_core::sanitize::sanitize_filename_for_windows;
///
/// assert_eq!(sanitize_filename_for_windows("normal.txt"), "normal.txt");
/// assert_eq!(sanitize_filename_for_windows("file:name.txt"), "file_name.txt");
/// assert_eq!(sanitize_filename_for_windows("CON.txt"), "_CON.txt");
/// assert_eq!(sanitize_filename_for_windows("file.txt."), "file.txt");
/// ```
pub fn sanitize_filename_for_windows(filename: &str) -> String {
    if filename.is_empty() {
        return "_unnamed_".to_string();
    }

    // Replace invalid characters with underscores
    let mut sanitized: String = filename
        .chars()
        .map(|c| {
            if WINDOWS_INVALID_CHARS.contains(&c) || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect();

    // Remove trailing dots and spaces (Windows strips these silently)
    while sanitized.ends_with('.') || sanitized.ends_with(' ') {
        sanitized.pop();
    }

    // Handle empty result after stripping
    if sanitized.is_empty() {
        return "_unnamed_".to_string();
    }

    // Check for reserved names (case-insensitive, with or without extension)
    let name_upper = sanitized.to_uppercase();
    let base_name = name_upper.split('.').next().unwrap_or("");

    if WINDOWS_RESERVED_NAMES.contains(&base_name) {
        sanitized = format!("_{}", sanitized);
    }

    // Truncate if too long (preserve extension if possible)
    if sanitized.len() > WINDOWS_MAX_FILENAME_LEN {
        if let Some(dot_pos) = sanitized.rfind('.') {
            let ext = &sanitized[dot_pos..];
            let ext_len = ext.len();
            if ext_len < WINDOWS_MAX_FILENAME_LEN - 1 {
                let base_max = WINDOWS_MAX_FILENAME_LEN - ext_len;
                let base = &sanitized[..dot_pos];
                // Truncate base, keeping extension
                let truncated_base: String = base.chars().take(base_max).collect();
                sanitized = format!("{}{}", truncated_base, ext);
            } else {
                // Extension itself is too long, just truncate
                sanitized = sanitized.chars().take(WINDOWS_MAX_FILENAME_LEN).collect();
            }
        } else {
            sanitized = sanitized.chars().take(WINDOWS_MAX_FILENAME_LEN).collect();
        }
    }

    sanitized
}

// =============================================================================
// Linux Filename Sanitization
// =============================================================================

/// Characters that are invalid in Linux filenames.
/// Only forward slash and null byte are truly invalid.
const LINUX_INVALID_CHARS: &[char] = &['/', '\0'];

/// Maximum filename length for most Linux filesystems.
const LINUX_MAX_FILENAME_LEN: usize = 255;

/// Sanitize a filename for use on Linux.
///
/// Linux is much more permissive than Windows, but we still handle:
/// - Forward slash (only truly invalid character besides null)
/// - Null bytes
/// - Backslashes (convert to underscores for safety with shell commands)
/// - Leading dashes (can be confused with command options)
/// - Truncation to 255 characters
///
/// # Arguments
///
/// * `filename` - The original filename (just the name, not a full path)
///
/// # Returns
///
/// A sanitized filename safe for use on Linux filesystems.
///
/// # Example
///
/// ```rust
/// use lamco_clipboard_core::sanitize::sanitize_filename_for_linux;
///
/// assert_eq!(sanitize_filename_for_linux("normal.txt"), "normal.txt");
/// assert_eq!(sanitize_filename_for_linux("path/file.txt"), "path_file.txt");
/// assert_eq!(sanitize_filename_for_linux("-dangerous"), "_-dangerous");
/// ```
pub fn sanitize_filename_for_linux(filename: &str) -> String {
    if filename.is_empty() {
        return "_unnamed_".to_string();
    }

    // Replace invalid characters
    let mut sanitized: String = filename
        .chars()
        .map(|c| {
            if LINUX_INVALID_CHARS.contains(&c) || c == '\\' {
                '_'
            } else {
                c
            }
        })
        .collect();

    // Handle leading dash (can be confused with command options)
    if sanitized.starts_with('-') {
        sanitized = format!("_{}", sanitized);
    }

    // Handle empty result
    if sanitized.is_empty() || sanitized == "." || sanitized == ".." {
        return "_unnamed_".to_string();
    }

    // Truncate if too long
    if sanitized.len() > LINUX_MAX_FILENAME_LEN {
        if let Some(dot_pos) = sanitized.rfind('.') {
            let ext = &sanitized[dot_pos..];
            let ext_len = ext.len();
            if ext_len < LINUX_MAX_FILENAME_LEN - 1 {
                let base_max = LINUX_MAX_FILENAME_LEN - ext_len;
                let base = &sanitized[..dot_pos];
                let truncated_base: String = base.chars().take(base_max).collect();
                sanitized = format!("{}{}", truncated_base, ext);
            } else {
                sanitized = sanitized.chars().take(LINUX_MAX_FILENAME_LEN).collect();
            }
        } else {
            sanitized = sanitized.chars().take(LINUX_MAX_FILENAME_LEN).collect();
        }
    }

    sanitized
}

// =============================================================================
// File URI Parsing
// =============================================================================

/// Parse file URIs from clipboard data.
///
/// Handles both standard `text/uri-list` format and GNOME's
/// `x-special/gnome-copied-files` format.
///
/// # Format Examples
///
/// **text/uri-list:**
/// ```text
/// file:///home/user/document.pdf
/// file:///home/user/image.png
/// ```
///
/// **x-special/gnome-copied-files:**
/// ```text
/// copy
/// file:///home/user/document.pdf
/// ```
///
/// # Arguments
///
/// * `data` - Raw clipboard data bytes
///
/// # Returns
///
/// A vector of parsed file paths. Invalid URIs or non-existent files are skipped.
///
/// # Example
///
/// ```rust
/// use lamco_clipboard_core::sanitize::parse_file_uris;
///
/// let data = b"file:///tmp/test.txt\nfile:///tmp/other.txt\n";
/// let paths = parse_file_uris(data);
/// // Returns paths that exist on the filesystem
/// ```
pub fn parse_file_uris(data: &[u8]) -> Vec<PathBuf> {
    let text = String::from_utf8_lossy(data);
    let mut paths = Vec::new();

    for line in text.lines() {
        let line = line.trim();

        // Skip empty lines and gnome-copied-files prefixes
        if line.is_empty() || line == "copy" || line == "cut" {
            continue;
        }

        // Parse file:// URI
        if let Some(path) = parse_file_uri(line) {
            if path.exists() {
                paths.push(path);
            } else {
                tracing::warn!("File URI points to non-existent path: {:?}", path);
            }
        }
    }

    paths
}

/// Parse a single file:// URI to a PathBuf.
///
/// Handles URL-encoded characters (e.g., `%20` for space).
///
/// # Arguments
///
/// * `uri` - A file:// URI string
///
/// # Returns
///
/// The decoded path, or None if the URI is invalid.
pub fn parse_file_uri(uri: &str) -> Option<PathBuf> {
    let path_str = uri.strip_prefix("file://")?;

    // URL decode the path
    let decoded = percent_decode(path_str);

    Some(PathBuf::from(decoded))
}

/// Simple percent-decoding for file URIs.
///
/// Decodes URL-encoded characters like `%20` (space), `%2F` (slash), etc.
fn percent_decode(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '%' {
            // Try to read two hex digits
            let hex: String = chars.by_ref().take(2).collect();
            if hex.len() == 2 {
                if let Ok(byte) = u8::from_str_radix(&hex, 16) {
                    // For multi-byte UTF-8, we need to collect bytes
                    // Simple case: ASCII character
                    if byte < 128 {
                        result.push(byte as char);
                        continue;
                    }
                    // For non-ASCII, decode as UTF-8 byte sequence
                    let mut bytes = vec![byte];
                    while chars.peek() == Some(&'%') {
                        chars.next(); // consume '%'
                        let hex2: String = chars.by_ref().take(2).collect();
                        if hex2.len() == 2 {
                            if let Ok(b) = u8::from_str_radix(&hex2, 16) {
                                bytes.push(b);
                                // Check if we have a complete UTF-8 sequence
                                if let Ok(s) = std::str::from_utf8(&bytes) {
                                    result.push_str(s);
                                    bytes.clear();
                                    break;
                                }
                            } else {
                                // Invalid hex, put back what we consumed
                                result.push('%');
                                result.push_str(&hex2);
                                break;
                            }
                        }
                    }
                    if !bytes.is_empty() {
                        // Incomplete UTF-8 sequence, use replacement char
                        result.push('\u{FFFD}');
                    }
                    continue;
                }
            }
            // Invalid percent encoding, keep literal
            result.push('%');
            result.push_str(&hex);
        } else {
            result.push(c);
        }
    }

    result
}

// =============================================================================
// Text Sanitization
// =============================================================================

/// Convert text line endings from Unix (LF) to Windows (CRLF).
///
/// This ensures text pasted in Windows applications displays correctly.
/// Already-present CRLF sequences are preserved (not doubled).
///
/// # Arguments
///
/// * `text` - The input text with Unix line endings
///
/// # Returns
///
/// Text with Windows-style line endings.
///
/// # Example
///
/// ```rust
/// use lamco_clipboard_core::sanitize::convert_line_endings_to_windows;
///
/// let unix = "line1\nline2\nline3";
/// let windows = convert_line_endings_to_windows(unix);
/// assert_eq!(windows, "line1\r\nline2\r\nline3");
/// ```
pub fn convert_line_endings_to_windows(text: &str) -> String {
    // First normalize any existing CRLF to LF, then convert all LF to CRLF
    let normalized = text.replace("\r\n", "\n");
    normalized.replace('\n', "\r\n")
}

/// Convert text line endings from Windows (CRLF) to Unix (LF).
///
/// This ensures text pasted in Linux applications displays correctly.
///
/// # Arguments
///
/// * `text` - The input text with Windows line endings
///
/// # Returns
///
/// Text with Unix-style line endings.
///
/// # Example
///
/// ```rust
/// use lamco_clipboard_core::sanitize::convert_line_endings_to_unix;
///
/// let windows = "line1\r\nline2\r\nline3";
/// let unix = convert_line_endings_to_unix(windows);
/// assert_eq!(unix, "line1\nline2\nline3");
/// ```
pub fn convert_line_endings_to_unix(text: &str) -> String {
    text.replace("\r\n", "\n")
}

/// Sanitize text for Windows clipboard.
///
/// This function:
/// - Converts line endings to CRLF
/// - Ensures valid UTF-8 (replaces invalid sequences)
/// - Removes null bytes
///
/// # Arguments
///
/// * `text` - The input text
///
/// # Returns
///
/// Sanitized text safe for Windows clipboard.
pub fn sanitize_text_for_windows(text: &str) -> String {
    let mut result = convert_line_endings_to_windows(text);
    // Remove any null bytes
    result.retain(|c| c != '\0');
    result
}

/// Sanitize text for Linux clipboard.
///
/// This function:
/// - Converts line endings to LF
/// - Ensures valid UTF-8 (replaces invalid sequences)
/// - Removes null bytes
///
/// # Arguments
///
/// * `text` - The input text
///
/// # Returns
///
/// Sanitized text safe for Linux clipboard.
pub fn sanitize_text_for_linux(text: &str) -> String {
    let mut result = convert_line_endings_to_unix(text);
    // Remove any null bytes
    result.retain(|c| c != '\0');
    result
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_filename_for_windows_basic() {
        assert_eq!(sanitize_filename_for_windows("normal.txt"), "normal.txt");
        assert_eq!(sanitize_filename_for_windows("file name.txt"), "file name.txt");
    }

    #[test]
    fn test_sanitize_filename_for_windows_invalid_chars() {
        assert_eq!(sanitize_filename_for_windows("file:name.txt"), "file_name.txt");
        assert_eq!(
            sanitize_filename_for_windows("a\\b/c:d*e?f\"g<h>i|j.txt"),
            "a_b_c_d_e_f_g_h_i_j.txt"
        );
    }

    #[test]
    fn test_sanitize_filename_for_windows_reserved_names() {
        assert_eq!(sanitize_filename_for_windows("CON"), "_CON");
        assert_eq!(sanitize_filename_for_windows("con.txt"), "_con.txt");
        assert_eq!(sanitize_filename_for_windows("COM1.log"), "_COM1.log");
        assert_eq!(sanitize_filename_for_windows("NUL"), "_NUL");
    }

    #[test]
    fn test_sanitize_filename_for_windows_trailing() {
        assert_eq!(sanitize_filename_for_windows("file.txt."), "file.txt");
        assert_eq!(sanitize_filename_for_windows("file.txt..."), "file.txt");
        assert_eq!(sanitize_filename_for_windows("file "), "file");
        assert_eq!(sanitize_filename_for_windows("..."), "_unnamed_");
    }

    #[test]
    fn test_sanitize_filename_for_windows_empty() {
        assert_eq!(sanitize_filename_for_windows(""), "_unnamed_");
    }

    #[test]
    fn test_sanitize_filename_for_linux_basic() {
        assert_eq!(sanitize_filename_for_linux("normal.txt"), "normal.txt");
        assert_eq!(sanitize_filename_for_linux("file:name.txt"), "file:name.txt");
        // Colons are OK on Linux
    }

    #[test]
    fn test_sanitize_filename_for_linux_slash() {
        assert_eq!(sanitize_filename_for_linux("path/file.txt"), "path_file.txt");
        assert_eq!(sanitize_filename_for_linux("back\\slash.txt"), "back_slash.txt");
    }

    #[test]
    fn test_sanitize_filename_for_linux_leading_dash() {
        assert_eq!(sanitize_filename_for_linux("-rf"), "_-rf");
        assert_eq!(sanitize_filename_for_linux("--help"), "_--help");
    }

    #[test]
    fn test_parse_file_uri() {
        assert_eq!(
            parse_file_uri("file:///home/user/test.txt"),
            Some(PathBuf::from("/home/user/test.txt"))
        );
        assert_eq!(
            parse_file_uri("file:///home/user/my%20file.txt"),
            Some(PathBuf::from("/home/user/my file.txt"))
        );
        assert_eq!(parse_file_uri("not-a-uri"), None);
        assert_eq!(parse_file_uri("http://example.com"), None);
    }

    #[test]
    fn test_percent_decode() {
        assert_eq!(percent_decode("hello%20world"), "hello world");
        assert_eq!(percent_decode("file%2Fname"), "file/name");
        assert_eq!(percent_decode("no-encoding"), "no-encoding");
        assert_eq!(percent_decode("%"), "%"); // Incomplete
    }

    #[test]
    fn test_convert_line_endings() {
        assert_eq!(convert_line_endings_to_windows("a\nb\nc"), "a\r\nb\r\nc");
        assert_eq!(convert_line_endings_to_windows("a\r\nb\nc"), "a\r\nb\r\nc");
        assert_eq!(convert_line_endings_to_unix("a\r\nb\r\nc"), "a\nb\nc");
    }
}
