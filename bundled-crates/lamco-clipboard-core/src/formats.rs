//! Clipboard format conversion utilities.
//!
//! This module handles conversion between MIME types and Windows clipboard format IDs,
//! as well as data conversion between formats.

use crate::{ClipboardError, ClipboardResult};

// =============================================================================
// Windows Clipboard Format IDs
// =============================================================================

/// Standard Windows clipboard format: Unicode text (UTF-16LE)
pub const CF_UNICODETEXT: u32 = 13;

/// Standard Windows clipboard format: ANSI text (Windows-1252 codepage)
pub const CF_TEXT: u32 = 1;

/// Standard Windows clipboard format: OEM text (DOS codepage)
/// Synthesized from CF_UNICODETEXT for very old applications
pub const CF_OEMTEXT: u32 = 7;

/// Standard Windows clipboard format: Device-independent bitmap
pub const CF_DIB: u32 = 8;

/// Standard Windows clipboard format: DIBV5 (Device-independent bitmap V5)
/// Extended bitmap format with alpha channel and color space support (124-byte header)
pub const CF_DIBV5: u32 = 17;

/// Standard Windows clipboard format: File drop list
pub const CF_HDROP: u32 = 15;

/// Standard Windows clipboard format: Wave audio
pub const CF_WAVE: u32 = 12;

/// Standard Windows clipboard format: RIFF audio
pub const CF_RIFF: u32 = 11;

/// Custom format: HTML (registered format name: "HTML Format")
pub const CF_HTML: u32 = 0xD010;

/// Custom format: PNG image
pub const CF_PNG: u32 = 0xD011;

/// Custom format: JPEG image
pub const CF_JPEG: u32 = 0xD012;

/// Custom format: GIF image
pub const CF_GIF: u32 = 0xD013;

/// Custom format: Rich Text Format
pub const CF_RTF: u32 = 0xD014;

/// File transfer format: FileGroupDescriptorW (registered format name)
/// Used for clipboard file transfer with delayed rendering (copy/paste, not drag/drop)
/// Contains metadata about files without actual data
pub const CF_FILEGROUPDESCRIPTORW: u32 = 49430;

/// File transfer format: FileContents (registered format name)
/// Used to retrieve actual file data chunks via FileContentsRequest/Response
pub const CF_FILECONTENTS: u32 = 49338;

// =============================================================================
// Clipboard Format
// =============================================================================

/// A clipboard format with ID and optional name
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ClipboardFormat {
    /// Windows clipboard format ID
    pub id: u32,

    /// Format name (for registered formats)
    pub name: Option<String>,
}

impl ClipboardFormat {
    /// Create a new clipboard format with ID only
    pub fn new(id: u32) -> Self {
        Self { id, name: None }
    }

    /// Create a new clipboard format with ID and name
    pub fn with_name(id: u32, name: impl Into<String>) -> Self {
        Self {
            id,
            name: Some(name.into()),
        }
    }

    /// Create format for Unicode text
    pub fn unicode_text() -> Self {
        Self::new(CF_UNICODETEXT)
    }

    /// Create format for HTML
    pub fn html() -> Self {
        Self::with_name(CF_HTML, "HTML Format")
    }

    /// Create format for PNG
    pub fn png() -> Self {
        Self::with_name(CF_PNG, "PNG")
    }

    /// Create format for file drop
    pub fn file_drop() -> Self {
        Self::new(CF_HDROP)
    }
}

// =============================================================================
// MIME <-> Format Conversion
// =============================================================================

/// Convert MIME types to RDP clipboard formats
///
/// # Example
///
/// ```
/// use lamco_clipboard_core::formats::mime_to_rdp_formats;
///
/// let formats = mime_to_rdp_formats(&["text/plain", "text/html"]);
/// assert!(!formats.is_empty());
/// ```
pub fn mime_to_rdp_formats(mime_types: &[&str]) -> Vec<ClipboardFormat> {
    let mut formats = Vec::new();

    for mime in mime_types {
        match *mime {
            // Text formats - announce all synthesized text formats for compatibility
            // Windows auto-synthesizes between these, but we announce all for maximum compatibility
            "text/plain" | "text/plain;charset=utf-8" | "UTF8_STRING" | "STRING" => {
                if !formats.iter().any(|f: &ClipboardFormat| f.id == CF_UNICODETEXT) {
                    // Primary format: Unicode (UTF-16LE)
                    formats.push(ClipboardFormat::unicode_text());
                    // Synthesized: ANSI text for legacy applications
                    formats.push(ClipboardFormat::new(CF_TEXT));
                    // Synthesized: OEM text for very old applications
                    formats.push(ClipboardFormat::new(CF_OEMTEXT));
                }
            }

            "text/html" => {
                formats.push(ClipboardFormat::html());
            }

            "text/rtf" | "application/rtf" => {
                formats.push(ClipboardFormat::with_name(CF_RTF, "Rich Text Format"));
            }

            // Image formats
            "image/png" => {
                formats.push(ClipboardFormat::png());
                // Also offer DIBV5 for alpha channel support (modern Windows apps prefer this)
                if !formats.iter().any(|f: &ClipboardFormat| f.id == CF_DIBV5) {
                    formats.push(ClipboardFormat::new(CF_DIBV5));
                }
                // Also offer DIB for legacy compatibility
                if !formats.iter().any(|f: &ClipboardFormat| f.id == CF_DIB) {
                    formats.push(ClipboardFormat::new(CF_DIB));
                }
            }

            "image/jpeg" | "image/jpg" => {
                formats.push(ClipboardFormat::with_name(CF_JPEG, "JFIF"));
                if !formats.iter().any(|f: &ClipboardFormat| f.id == CF_DIB) {
                    formats.push(ClipboardFormat::new(CF_DIB));
                }
            }

            "image/gif" => {
                formats.push(ClipboardFormat::with_name(CF_GIF, "GIF"));
            }

            "image/bmp" | "image/x-bmp" => {
                formats.push(ClipboardFormat::new(CF_DIB));
            }

            // File formats - use RDP registered formats for clipboard file transfer
            "text/uri-list" | "x-special/gnome-copied-files" => {
                // For RDP file transfer, we need FileGroupDescriptorW (file list metadata)
                // and FileContents (actual file data retrieval)
                // ID 0 means it's a registered format - the name is what matters
                if !formats
                    .iter()
                    .any(|f: &ClipboardFormat| f.name.as_ref().is_some_and(|n| n == "FileGroupDescriptorW"))
                {
                    formats.push(ClipboardFormat::with_name(0, "FileGroupDescriptorW"));
                    formats.push(ClipboardFormat::with_name(0, "FileContents"));
                }
            }

            // Audio formats
            "audio/wav" | "audio/x-wav" => {
                formats.push(ClipboardFormat::new(CF_WAVE));
            }

            _ => {
                // Unknown format - skip
                tracing::debug!("Unknown MIME type: {}", mime);
            }
        }
    }

    formats
}

/// Convert RDP format ID to preferred MIME type
///
/// # Example
///
/// ```
/// use lamco_clipboard_core::formats::{rdp_format_to_mime, CF_UNICODETEXT};
///
/// let mime = rdp_format_to_mime(CF_UNICODETEXT);
/// assert_eq!(mime, Some("text/plain;charset=utf-8"));
/// ```
pub fn rdp_format_to_mime(format_id: u32) -> Option<&'static str> {
    match format_id {
        // All text formats map to the same MIME type - we'll convert encoding as needed
        CF_UNICODETEXT | CF_TEXT | CF_OEMTEXT => Some("text/plain;charset=utf-8"),
        CF_HTML => Some("text/html"),
        CF_RTF => Some("text/rtf"),
        CF_DIB | CF_DIBV5 => Some("image/png"), // Prefer PNG output (preserves alpha from DIBV5)
        CF_PNG => Some("image/png"),
        CF_JPEG => Some("image/jpeg"),
        CF_GIF => Some("image/gif"),
        CF_HDROP | CF_FILEGROUPDESCRIPTORW => Some("text/uri-list"),
        CF_WAVE | CF_RIFF => Some("audio/wav"),
        // CF_FILECONTENTS is not mapped to MIME - it's a data retrieval mechanism, not a format
        _ => None,
    }
}

// =============================================================================
// Format Converter
// =============================================================================

/// Handles clipboard data format conversion
#[derive(Debug, Default)]
pub struct FormatConverter {
    /// Maximum data size for conversion (default: 16MB)
    pub max_size: usize,
}

impl FormatConverter {
    /// Create a new format converter with default settings
    pub fn new() -> Self {
        Self {
            max_size: 16 * 1024 * 1024, // 16MB
        }
    }

    /// Create a format converter with custom max size
    pub fn with_max_size(max_size: usize) -> Self {
        Self { max_size }
    }

    /// Convert UTF-8 text to UTF-16LE (for CF_UNICODETEXT)
    ///
    /// Adds null terminator as required by Windows.
    pub fn text_to_unicode(&self, text: &str) -> ClipboardResult<Vec<u8>> {
        if text.len() > self.max_size {
            return Err(ClipboardError::DataSizeExceeded {
                actual: text.len(),
                max: self.max_size,
            });
        }

        let mut result: Vec<u8> = text.encode_utf16().flat_map(|c| c.to_le_bytes()).collect();

        // Add null terminator (2 bytes for UTF-16)
        result.extend_from_slice(&[0, 0]);

        Ok(result)
    }

    /// Convert UTF-16LE to UTF-8 (from CF_UNICODETEXT)
    pub fn unicode_to_text(&self, data: &[u8]) -> ClipboardResult<String> {
        if data.len() > self.max_size {
            return Err(ClipboardError::DataSizeExceeded {
                actual: data.len(),
                max: self.max_size,
            });
        }

        if data.len() % 2 != 0 {
            return Err(ClipboardError::InvalidUtf16);
        }

        let utf16: Vec<u16> = data
            .chunks_exact(2)
            .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
            .collect();

        // Remove null terminator if present
        let utf16 = if utf16.last() == Some(&0) {
            &utf16[..utf16.len() - 1]
        } else {
            &utf16[..]
        };

        String::from_utf16(utf16).map_err(|_| ClipboardError::InvalidUtf16)
    }

    /// Convert UTF-8 text to ANSI (Windows-1252) for CF_TEXT
    ///
    /// Characters not representable in Windows-1252 are replaced with '?'.
    /// Adds null terminator as required by Windows.
    pub fn text_to_ansi(&self, text: &str) -> ClipboardResult<Vec<u8>> {
        if text.len() > self.max_size {
            return Err(ClipboardError::DataSizeExceeded {
                actual: text.len(),
                max: self.max_size,
            });
        }

        let mut result = Vec::with_capacity(text.len() + 1);

        for c in text.chars() {
            result.push(char_to_windows1252(c));
        }

        // Add null terminator
        result.push(0);

        Ok(result)
    }

    /// Convert ANSI (Windows-1252) to UTF-8 (from CF_TEXT)
    pub fn ansi_to_text(&self, data: &[u8]) -> ClipboardResult<String> {
        if data.len() > self.max_size {
            return Err(ClipboardError::DataSizeExceeded {
                actual: data.len(),
                max: self.max_size,
            });
        }

        // Remove null terminator if present
        let data = if data.last() == Some(&0) {
            &data[..data.len() - 1]
        } else {
            data
        };

        let result: String = data.iter().map(|&b| windows1252_to_char(b)).collect();
        Ok(result)
    }

    /// Convert UTF-8 text to OEM (CP437) for CF_OEMTEXT
    ///
    /// Characters not representable in CP437 are replaced with '?'.
    /// Adds null terminator as required by Windows.
    pub fn text_to_oem(&self, text: &str) -> ClipboardResult<Vec<u8>> {
        if text.len() > self.max_size {
            return Err(ClipboardError::DataSizeExceeded {
                actual: text.len(),
                max: self.max_size,
            });
        }

        let mut result = Vec::with_capacity(text.len() + 1);

        for c in text.chars() {
            result.push(char_to_cp437(c));
        }

        // Add null terminator
        result.push(0);

        Ok(result)
    }

    /// Convert OEM (CP437) to UTF-8 (from CF_OEMTEXT)
    pub fn oem_to_text(&self, data: &[u8]) -> ClipboardResult<String> {
        if data.len() > self.max_size {
            return Err(ClipboardError::DataSizeExceeded {
                actual: data.len(),
                max: self.max_size,
            });
        }

        // Remove null terminator if present
        let data = if data.last() == Some(&0) {
            &data[..data.len() - 1]
        } else {
            data
        };

        let result: String = data.iter().map(|&b| cp437_to_char(b)).collect();
        Ok(result)
    }

    /// Convert plain HTML to Windows CF_HTML format
    ///
    /// The CF_HTML format includes headers with byte offsets.
    pub fn html_to_cf_html(&self, html: &str) -> ClipboardResult<Vec<u8>> {
        if html.len() > self.max_size {
            return Err(ClipboardError::DataSizeExceeded {
                actual: html.len(),
                max: self.max_size,
            });
        }

        // CF_HTML format:
        // Version:0.9
        // StartHTML:XXXXXXXX
        // EndHTML:XXXXXXXX
        // StartFragment:XXXXXXXX
        // EndFragment:XXXXXXXX
        // <html><body><!--StartFragment-->CONTENT<!--EndFragment--></body></html>

        let header_template = "Version:0.9\r\n\
                               StartHTML:XXXXXXXX\r\n\
                               EndHTML:XXXXXXXX\r\n\
                               StartFragment:XXXXXXXX\r\n\
                               EndFragment:XXXXXXXX\r\n";

        let prefix = "<html><body><!--StartFragment-->";
        let suffix = "<!--EndFragment--></body></html>";

        let header_len = header_template.len();
        let start_html = header_len;
        let start_fragment = header_len + prefix.len();
        let end_fragment = start_fragment + html.len();
        let end_html = end_fragment + suffix.len();

        let header = format!(
            "Version:0.9\r\n\
             StartHTML:{:08}\r\n\
             EndHTML:{:08}\r\n\
             StartFragment:{:08}\r\n\
             EndFragment:{:08}\r\n",
            start_html, end_html, start_fragment, end_fragment
        );

        let mut result = header;
        result.push_str(prefix);
        result.push_str(html);
        result.push_str(suffix);

        Ok(result.into_bytes())
    }

    /// Extract HTML content from CF_HTML format
    pub fn cf_html_to_html(&self, data: &[u8]) -> ClipboardResult<String> {
        let text = std::str::from_utf8(data).map_err(|_| ClipboardError::InvalidUtf8)?;

        // Parse StartFragment and EndFragment from header
        let start_fragment = Self::parse_header_value(text, "StartFragment:")?;
        let end_fragment = Self::parse_header_value(text, "EndFragment:")?;

        if start_fragment >= end_fragment || end_fragment > data.len() {
            return Err(ClipboardError::FormatConversion("invalid CF_HTML offsets".to_string()));
        }

        let fragment = &text[start_fragment..end_fragment];
        Ok(fragment.to_string())
    }

    /// Parse a numeric header value from CF_HTML
    fn parse_header_value(text: &str, key: &str) -> ClipboardResult<usize> {
        text.lines()
            .find(|line| line.starts_with(key))
            .and_then(|line| line[key.len()..].trim().parse().ok())
            .ok_or_else(|| ClipboardError::FormatConversion(format!("missing {} header", key)))
    }

    // =========================================================================
    // RTF Format Support
    // =========================================================================

    /// Validate and pass through RTF data
    ///
    /// RTF (Rich Text Format) is passed through without conversion since both
    /// Windows and Linux applications understand it natively. This method validates
    /// the RTF header and returns the data unchanged.
    ///
    /// # Arguments
    /// * `data` - Raw RTF data (must start with `{\rtf`)
    ///
    /// # Returns
    /// * The validated RTF data, or error if invalid
    pub fn validate_rtf(&self, data: &[u8]) -> ClipboardResult<Vec<u8>> {
        if data.len() > self.max_size {
            return Err(ClipboardError::DataSizeExceeded {
                actual: data.len(),
                max: self.max_size,
            });
        }

        // RTF documents must start with {\rtf
        if !data.starts_with(b"{\\rtf") {
            return Err(ClipboardError::FormatConversion(
                "Invalid RTF: must start with {\\rtf".to_string(),
            ));
        }

        // Basic brace matching check
        let mut depth = 0i32;
        for &byte in data {
            match byte {
                b'{' => depth += 1,
                b'}' => depth -= 1,
                _ => {}
            }
            if depth < 0 {
                return Err(ClipboardError::FormatConversion(
                    "Invalid RTF: unmatched closing brace".to_string(),
                ));
            }
        }

        if depth != 0 {
            return Err(ClipboardError::FormatConversion(
                "Invalid RTF: unmatched braces".to_string(),
            ));
        }

        Ok(data.to_vec())
    }

    /// Check if data looks like valid RTF
    ///
    /// Quick validation without full parsing - useful for format detection.
    pub fn is_rtf(&self, data: &[u8]) -> bool {
        data.starts_with(b"{\\rtf")
    }

    /// Convert plain text to minimal RTF
    ///
    /// Creates a simple RTF document from plain text. Useful when RTF is requested
    /// but only plain text is available.
    pub fn text_to_rtf(&self, text: &str) -> ClipboardResult<Vec<u8>> {
        if text.len() > self.max_size {
            return Err(ClipboardError::DataSizeExceeded {
                actual: text.len(),
                max: self.max_size,
            });
        }

        let mut rtf = String::with_capacity(text.len() + 100);

        // RTF header: version 1, ANSI charset, default font
        rtf.push_str("{\\rtf1\\ansi\\deff0\n");

        // Font table with a basic font
        rtf.push_str("{\\fonttbl{\\f0\\fswiss\\fcharset0 Arial;}}\n");

        // Content
        for c in text.chars() {
            match c {
                '\\' => rtf.push_str("\\\\"),
                '{' => rtf.push_str("\\{"),
                '}' => rtf.push_str("\\}"),
                '\n' => rtf.push_str("\\par\n"),
                '\r' => {} // Skip CR, \n handles line breaks
                c if c.is_ascii() => rtf.push(c),
                c => {
                    // Unicode escape: \uN?
                    // The ? is the fallback character for non-Unicode readers
                    rtf.push_str(&format!("\\u{}?", c as u32));
                }
            }
        }

        rtf.push('}');

        Ok(rtf.into_bytes())
    }

    /// Extract plain text from RTF
    ///
    /// Performs basic RTF parsing to extract readable text content.
    /// This is a simplified parser that handles common cases.
    pub fn rtf_to_text(&self, data: &[u8]) -> ClipboardResult<String> {
        if data.len() > self.max_size {
            return Err(ClipboardError::DataSizeExceeded {
                actual: data.len(),
                max: self.max_size,
            });
        }

        let text = std::str::from_utf8(data).map_err(|_| ClipboardError::InvalidUtf8)?;

        let mut result = String::new();
        let mut chars = text.chars().peekable();
        let mut skip_depth: Option<i32> = None; // Depth at which we started skipping
        let mut group_depth = 0i32;

        while let Some(c) = chars.next() {
            match c {
                '{' => {
                    group_depth += 1;
                }
                '}' => {
                    // If we were skipping and we're back to the skip start depth, stop skipping
                    if let Some(sd) = skip_depth {
                        if group_depth == sd {
                            skip_depth = None;
                        }
                    }
                    group_depth -= 1;
                }
                '\\' => {
                    // Parse control word
                    let mut control_word = String::new();
                    while let Some(&nc) = chars.peek() {
                        if nc.is_ascii_alphabetic() {
                            control_word.push(chars.next().unwrap());
                        } else {
                            break;
                        }
                    }

                    // Skip numeric parameter if present
                    let mut has_param = false;
                    while let Some(&nc) = chars.peek() {
                        if nc.is_ascii_digit() || nc == '-' {
                            chars.next();
                            has_param = true;
                        } else {
                            break;
                        }
                    }

                    // Consume trailing space if present (part of control word)
                    if chars.peek() == Some(&' ') && !has_param {
                        chars.next();
                    }

                    // Check for destination groups to skip
                    // These are RTF groups that contain metadata, not document text
                    let skip_destinations = [
                        "fonttbl",
                        "colortbl",
                        "stylesheet",
                        "info",
                        "pict",
                        "header",
                        "footer",
                        "footnote",
                        "annotation",
                        "field",
                        "fldinst",
                        "datafield",
                        "docvar",
                        "xe",
                        "tc",
                        "rxe",
                    ];

                    if skip_destinations.contains(&control_word.as_str()) {
                        skip_depth = Some(group_depth);
                        continue;
                    }

                    // Skip if we're in a destination group
                    if skip_depth.is_some() {
                        continue;
                    }

                    // Handle common control words
                    match control_word.as_str() {
                        "par" | "line" => result.push('\n'),
                        "tab" => result.push('\t'),
                        "" => {
                            // Escaped character
                            if let Some(escaped) = chars.next() {
                                match escaped {
                                    '\\' | '{' | '}' => result.push(escaped),
                                    '\'' => {
                                        // Hex character \'xx
                                        let hex: String = chars.by_ref().take(2).collect();
                                        if let Ok(byte) = u8::from_str_radix(&hex, 16) {
                                            result.push(byte as char);
                                        }
                                    }
                                    '*' => {
                                        // \* marks a destination - skip until end of current group
                                        skip_depth = Some(group_depth);
                                    }
                                    _ => {}
                                }
                            }
                        }
                        _ => {} // Ignore other control words
                    }
                }
                _ if skip_depth.is_none() && c >= ' ' => {
                    result.push(c);
                }
                _ => {}
            }
        }

        Ok(result)
    }

    /// Convert URI list to HDROP format (file paths)
    ///
    /// The HDROP format is a DROPFILES structure followed by null-terminated paths.
    pub fn uri_list_to_hdrop(&self, uri_list: &str) -> ClipboardResult<Vec<u8>> {
        let paths: Vec<&str> = uri_list
            .lines()
            .filter(|line| !line.starts_with('#'))
            .filter_map(|line| line.strip_prefix("file://"))
            .collect();

        if paths.is_empty() {
            return Err(ClipboardError::FormatConversion("no valid file URIs".to_string()));
        }

        // DROPFILES structure (20 bytes):
        // DWORD pFiles (offset to file list)
        // POINT pt (unused, 8 bytes)
        // BOOL fNC (unused, 4 bytes)
        // BOOL fWide (TRUE for Unicode)

        let mut result = Vec::new();

        // pFiles: offset 20 (size of DROPFILES)
        result.extend_from_slice(&20u32.to_le_bytes());
        // pt.x, pt.y (unused)
        result.extend_from_slice(&0i32.to_le_bytes());
        result.extend_from_slice(&0i32.to_le_bytes());
        // fNC (unused)
        result.extend_from_slice(&0u32.to_le_bytes());
        // fWide = TRUE (Unicode paths)
        result.extend_from_slice(&1u32.to_le_bytes());

        // File paths as UTF-16LE, null-terminated
        for path in paths {
            // URL decode the path
            let decoded = percent_decode(path);
            for c in decoded.encode_utf16() {
                result.extend_from_slice(&c.to_le_bytes());
            }
            // Null terminator
            result.extend_from_slice(&[0, 0]);
        }

        // Final double null terminator
        result.extend_from_slice(&[0, 0]);

        Ok(result)
    }

    /// Convert HDROP format to URI list
    pub fn hdrop_to_uri_list(&self, data: &[u8]) -> ClipboardResult<String> {
        if data.len() < 20 {
            return Err(ClipboardError::FormatConversion("HDROP too small".to_string()));
        }

        // Read DROPFILES header
        let p_files = u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as usize;
        let f_wide = u32::from_le_bytes([data[16], data[17], data[18], data[19]]) != 0;

        if p_files >= data.len() {
            return Err(ClipboardError::FormatConversion("invalid pFiles offset".to_string()));
        }

        let mut paths = Vec::new();
        let file_data = &data[p_files..];

        if f_wide {
            // UTF-16LE paths
            let mut pos = 0;
            while pos + 2 <= file_data.len() {
                let mut path_chars = Vec::new();
                while pos + 2 <= file_data.len() {
                    let c = u16::from_le_bytes([file_data[pos], file_data[pos + 1]]);
                    pos += 2;
                    if c == 0 {
                        break;
                    }
                    path_chars.push(c);
                }

                if path_chars.is_empty() {
                    break; // Double null = end
                }

                if let Ok(path) = String::from_utf16(&path_chars) {
                    paths.push(format!("file://{}", percent_encode(&path)));
                }
            }
        } else {
            // ANSI paths (rare)
            let mut pos = 0;
            while pos < file_data.len() {
                let end = file_data[pos..]
                    .iter()
                    .position(|&b| b == 0)
                    .unwrap_or(file_data.len() - pos);
                if end == 0 {
                    break;
                }
                if let Ok(path) = std::str::from_utf8(&file_data[pos..pos + end]) {
                    paths.push(format!("file://{}", percent_encode(path)));
                }
                pos += end + 1;
            }
        }

        Ok(paths.join("\r\n"))
    }
}

// =============================================================================
// URL Encoding Helpers
// =============================================================================

/// Percent-decode a URL path
fn percent_decode(input: &str) -> String {
    let mut result = String::new();
    let mut chars = input.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '%' {
            let hex: String = chars.by_ref().take(2).collect();
            if let Ok(byte) = u8::from_str_radix(&hex, 16) {
                result.push(byte as char);
            } else {
                result.push('%');
                result.push_str(&hex);
            }
        } else {
            result.push(c);
        }
    }

    result
}

/// Percent-encode special characters in a path
fn percent_encode(input: &str) -> String {
    let mut result = String::new();

    for c in input.chars() {
        match c {
            ' ' => result.push_str("%20"),
            '#' => result.push_str("%23"),
            '%' => result.push_str("%25"),
            '?' => result.push_str("%3F"),
            _ => result.push(c),
        }
    }

    result
}

// =============================================================================
// File Transfer Structures
// =============================================================================

/// Windows file descriptor flags
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileDescriptorFlags(u32);

impl FileDescriptorFlags {
    /// File attributes are present
    pub const ATTRIBUTES: u32 = 0x00000001;
    /// File size is present
    pub const FILESIZE: u32 = 0x00000040;
    /// Write time is present
    pub const WRITESTIME: u32 = 0x00000020;
    /// Creation time is present
    pub const CREATETIME: u32 = 0x00000002;
    /// Access time is present
    pub const ACCESSTIME: u32 = 0x00000010;

    /// Create from raw flags value
    pub fn from_raw(flags: u32) -> Self {
        Self(flags)
    }

    /// Check if a flag is set
    pub fn has_flag(&self, flag: u32) -> bool {
        (self.0 & flag) != 0
    }
}

/// File descriptor from FileGroupDescriptorW structure
///
/// Represents a single file in a clipboard file transfer operation.
/// Parsed from the 88-byte FILEDESCRIPTORW Windows structure.
#[derive(Debug, Clone)]
pub struct FileDescriptor {
    /// File descriptor flags indicating which fields are valid
    pub flags: FileDescriptorFlags,

    /// File attributes (Windows FILE_ATTRIBUTE_*)
    pub attributes: u32,

    /// File creation time (Windows FILETIME format - 100ns intervals since 1601-01-01)
    pub creation_time: Option<u64>,

    /// File last access time
    pub access_time: Option<u64>,

    /// File last write time
    pub write_time: Option<u64>,

    /// File size in bytes
    pub size: Option<u64>,

    /// File name (UTF-16 decoded to UTF-8, max 260 characters)
    pub name: String,
}

impl FileDescriptor {
    /// Parse a single FILEDESCRIPTORW structure from bytes
    ///
    /// # Format (88 bytes total)
    /// ```text
    /// Offset | Size | Field
    /// -------|------|------
    /// 0      | 4    | dwFlags
    /// 4      | 16   | clsid (GUID, unused)
    /// 20     | 8    | sizel (SIZE, unused)
    /// 28     | 8    | pointl (POINT, unused)
    /// 36     | 4    | dwFileAttributes
    /// 40     | 8    | ftCreationTime
    /// 48     | 8    | ftLastAccessTime
    /// 56     | 8    | ftLastWriteTime
    /// 64     | 8    | nFileSize (split into High:Low)
    /// 72     | 520  | cFileName (UTF-16, 260 chars max)
    /// ```
    pub fn parse(data: &[u8]) -> ClipboardResult<Self> {
        if data.len() < 592 {
            return Err(ClipboardError::FormatConversion(format!(
                "FILEDESCRIPTORW too small: {} bytes (need 592)",
                data.len()
            )));
        }

        // Parse flags (offset 0, 4 bytes)
        let flags = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        let flags = FileDescriptorFlags::from_raw(flags);

        // Parse file attributes (offset 36, 4 bytes)
        let attributes = u32::from_le_bytes([data[36], data[37], data[38], data[39]]);

        // Parse creation time (offset 40, 8 bytes) if flag set
        let creation_time = if flags.has_flag(FileDescriptorFlags::CREATETIME) {
            Some(u64::from_le_bytes([
                data[40], data[41], data[42], data[43], data[44], data[45], data[46], data[47],
            ]))
        } else {
            None
        };

        // Parse access time (offset 48, 8 bytes) if flag set
        let access_time = if flags.has_flag(FileDescriptorFlags::ACCESSTIME) {
            Some(u64::from_le_bytes([
                data[48], data[49], data[50], data[51], data[52], data[53], data[54], data[55],
            ]))
        } else {
            None
        };

        // Parse write time (offset 56, 8 bytes) if flag set
        let write_time = if flags.has_flag(FileDescriptorFlags::WRITESTIME) {
            Some(u64::from_le_bytes([
                data[56], data[57], data[58], data[59], data[60], data[61], data[62], data[63],
            ]))
        } else {
            None
        };

        // Parse file size (offset 64, 8 bytes: nFileSizeHigh then nFileSizeLow) if flag set
        let size = if flags.has_flag(FileDescriptorFlags::FILESIZE) {
            let size_high = u32::from_le_bytes([data[64], data[65], data[66], data[67]]);
            let size_low = u32::from_le_bytes([data[68], data[69], data[70], data[71]]);
            Some(((size_high as u64) << 32) | (size_low as u64))
        } else {
            None
        };

        // Parse filename (offset 72, 520 bytes = 260 UTF-16 characters)
        let filename_bytes = &data[72..592];
        let name = Self::parse_utf16_filename(filename_bytes)?;

        Ok(FileDescriptor {
            flags,
            attributes,
            creation_time,
            access_time,
            write_time,
            size,
            name,
        })
    }

    /// Parse UTF-16LE filename from raw bytes
    fn parse_utf16_filename(data: &[u8]) -> ClipboardResult<String> {
        if data.len() % 2 != 0 {
            return Err(ClipboardError::InvalidUtf16);
        }

        let utf16: Vec<u16> = data
            .chunks_exact(2)
            .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
            .take_while(|&c| c != 0) // Stop at null terminator
            .collect();

        String::from_utf16(&utf16).map_err(|_| ClipboardError::InvalidUtf16)
    }

    /// Parse a list of file descriptors from FileGroupDescriptorW data
    ///
    /// # Format
    /// ```text
    /// Offset | Size | Field
    /// -------|------|------
    /// 0      | 4    | cItems (number of descriptors)
    /// 4      | 592  | fgd[0] (first FILEDESCRIPTORW)
    /// 596    | 592  | fgd[1] (second FILEDESCRIPTORW)
    /// ...
    /// ```
    pub fn parse_list(data: &[u8]) -> ClipboardResult<Vec<Self>> {
        if data.len() < 4 {
            return Err(ClipboardError::FormatConversion(
                "FileGroupDescriptorW too small for count".to_string(),
            ));
        }

        // First 4 bytes: number of descriptors
        let count = u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as usize;

        if count == 0 {
            return Ok(Vec::new());
        }

        // Validate total size
        let expected_size = 4 + (count * 592);
        if data.len() < expected_size {
            return Err(ClipboardError::FormatConversion(format!(
                "FileGroupDescriptorW too small: {} bytes (need {} for {} files)",
                data.len(),
                expected_size,
                count
            )));
        }

        // Parse each descriptor
        let mut descriptors = Vec::with_capacity(count);
        for i in 0..count {
            let offset = 4 + (i * 592);
            let descriptor_data = &data[offset..offset + 592];
            descriptors.push(Self::parse(descriptor_data)?);
        }

        Ok(descriptors)
    }

    /// Build a single FILEDESCRIPTORW structure for a file
    ///
    /// Returns 592 bytes representing the file descriptor.
    /// The filename is sanitized for Windows compatibility.
    pub fn build(path: &std::path::Path) -> ClipboardResult<Vec<u8>> {
        let metadata = std::fs::metadata(path)
            .map_err(|e| ClipboardError::FormatConversion(format!("Failed to get file metadata: {}", e)))?;

        let raw_filename = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| ClipboardError::FormatConversion("Invalid filename".to_string()))?;

        // Sanitize filename for Windows compatibility
        let filename = crate::sanitize::sanitize_filename_for_windows(raw_filename);

        let mut data = vec![0u8; 592];

        // Set flags: we provide file size
        let flags = FileDescriptorFlags::FILESIZE;
        data[0..4].copy_from_slice(&flags.to_le_bytes());

        // File attributes (offset 36) - normal file
        let attributes: u32 = if metadata.is_dir() { 0x10 } else { 0x80 }; // FILE_ATTRIBUTE_DIRECTORY or FILE_ATTRIBUTE_NORMAL
        data[36..40].copy_from_slice(&attributes.to_le_bytes());

        // File size (offset 64-71: nFileSizeHigh, nFileSizeLow)
        let size = metadata.len();
        let size_high = (size >> 32) as u32;
        let size_low = size as u32;
        data[64..68].copy_from_slice(&size_high.to_le_bytes());
        data[68..72].copy_from_slice(&size_low.to_le_bytes());

        // Filename (offset 72, 520 bytes = 260 UTF-16 characters)
        let filename_utf16: Vec<u16> = filename.encode_utf16().collect();
        let filename_len = filename_utf16.len().min(259); // Leave room for null terminator
        for (i, &c) in filename_utf16.iter().take(filename_len).enumerate() {
            let offset = 72 + i * 2;
            data[offset..offset + 2].copy_from_slice(&c.to_le_bytes());
        }
        // Null terminator already present (data was initialized to 0)

        Ok(data)
    }

    /// Build FileGroupDescriptorW data from a list of file paths
    ///
    /// # Format
    /// ```text
    /// Offset | Size | Field
    /// -------|------|------
    /// 0      | 4    | cItems (number of descriptors)
    /// 4      | 592  | fgd[0] (first FILEDESCRIPTORW)
    /// 596    | 592  | fgd[1] (second FILEDESCRIPTORW)
    /// ...
    /// ```
    pub fn build_list(paths: &[std::path::PathBuf]) -> ClipboardResult<Vec<u8>> {
        let count = paths.len() as u32;
        let mut data = Vec::with_capacity(4 + paths.len() * 592);

        // Write count (4 bytes)
        data.extend_from_slice(&count.to_le_bytes());

        // Write each descriptor
        for path in paths {
            let descriptor = Self::build(path)?;
            data.extend_from_slice(&descriptor);
        }

        Ok(data)
    }
}

/// Build FileGroupDescriptorW data from a list of file paths
///
/// This is a convenience function that calls FileDescriptor::build_list.
pub fn build_file_group_descriptor_w(paths: &[std::path::PathBuf]) -> ClipboardResult<Vec<u8>> {
    FileDescriptor::build_list(paths)
}

// =============================================================================
// Codepage Conversion Helpers (Synthesized Format Support)
// =============================================================================

/// Convert a Unicode character to Windows-1252 (Western European)
///
/// Returns '?' for characters not representable in Windows-1252.
fn char_to_windows1252(c: char) -> u8 {
    let cp = c as u32;

    // ASCII range (0-127) maps directly
    if cp < 128 {
        return cp as u8;
    }

    // Windows-1252 specific mappings (128-159 range has special characters)
    // 160-255 mostly match Latin-1 Supplement
    match cp {
        // 128-159: Windows-1252 specific characters
        0x20AC => 128, // €
        0x201A => 130, // ‚
        0x0192 => 131, // ƒ
        0x201E => 132, // „
        0x2026 => 133, // …
        0x2020 => 134, // †
        0x2021 => 135, // ‡
        0x02C6 => 136, // ˆ
        0x2030 => 137, // ‰
        0x0160 => 138, // Š
        0x2039 => 139, // ‹
        0x0152 => 140, // Œ
        0x017D => 142, // Ž
        0x2018 => 145, // '
        0x2019 => 146, // '
        0x201C => 147, // "
        0x201D => 148, // "
        0x2022 => 149, // •
        0x2013 => 150, // –
        0x2014 => 151, // —
        0x02DC => 152, // ˜
        0x2122 => 153, // ™
        0x0161 => 154, // š
        0x203A => 155, // ›
        0x0153 => 156, // œ
        0x017E => 158, // ž
        0x0178 => 159, // Ÿ
        // 160-255: Latin-1 Supplement (direct mapping)
        160..=255 => cp as u8,
        // Not representable
        _ => b'?',
    }
}

/// Convert a Windows-1252 byte to Unicode character
fn windows1252_to_char(b: u8) -> char {
    // ASCII range maps directly
    if b < 128 {
        return b as char;
    }

    // 160-255 range matches Latin-1 Supplement
    if b >= 160 {
        return char::from_u32(b as u32).unwrap_or('?');
    }

    // 128-159: Windows-1252 specific characters
    match b {
        128 => '€',
        130 => '‚',
        131 => 'ƒ',
        132 => '„',
        133 => '…',
        134 => '†',
        135 => '‡',
        136 => 'ˆ',
        137 => '‰',
        138 => 'Š',
        139 => '‹',
        140 => 'Œ',
        142 => 'Ž',
        145 => '\u{2018}', // '
        146 => '\u{2019}', // '
        147 => '\u{201C}', // "
        148 => '\u{201D}', // "
        149 => '•',
        150 => '–',
        151 => '—',
        152 => '˜',
        153 => '™',
        154 => 'š',
        155 => '›',
        156 => 'œ',
        158 => 'ž',
        159 => 'Ÿ',
        // Undefined positions (129, 141, 143, 144, 157)
        _ => '?',
    }
}

/// Convert a Unicode character to CP437 (OEM/DOS codepage)
///
/// Returns '?' for characters not representable in CP437.
fn char_to_cp437(c: char) -> u8 {
    let cp = c as u32;

    // ASCII printable range (32-126) maps directly
    if (32..127).contains(&cp) {
        return cp as u8;
    }

    // Control characters (0-31) - map directly for compatibility
    if cp < 32 {
        return cp as u8;
    }

    // CP437 high characters (128-255) - common ones
    match cp {
        0x00C7 => 128, // Ç
        0x00FC => 129, // ü
        0x00E9 => 130, // é
        0x00E2 => 131, // â
        0x00E4 => 132, // ä
        0x00E0 => 133, // à
        0x00E5 => 134, // å
        0x00E7 => 135, // ç
        0x00EA => 136, // ê
        0x00EB => 137, // ë
        0x00E8 => 138, // è
        0x00EF => 139, // ï
        0x00EE => 140, // î
        0x00EC => 141, // ì
        0x00C4 => 142, // Ä
        0x00C5 => 143, // Å
        0x00C9 => 144, // É
        0x00E6 => 145, // æ
        0x00C6 => 146, // Æ
        0x00F4 => 147, // ô
        0x00F6 => 148, // ö
        0x00F2 => 149, // ò
        0x00FB => 150, // û
        0x00F9 => 151, // ù
        0x00FF => 152, // ÿ
        0x00D6 => 153, // Ö
        0x00DC => 154, // Ü
        0x00A2 => 155, // ¢
        0x00A3 => 156, // £
        0x00A5 => 157, // ¥
        0x20A7 => 158, // ₧
        0x0192 => 159, // ƒ
        0x00E1 => 160, // á
        0x00ED => 161, // í
        0x00F3 => 162, // ó
        0x00FA => 163, // ú
        0x00F1 => 164, // ñ
        0x00D1 => 165, // Ñ
        0x00AA => 166, // ª
        0x00BA => 167, // º
        0x00BF => 168, // ¿
        0x00A1 => 173, // ¡
        0x00AB => 174, // «
        0x00BB => 175, // »
        0x00B0 => 248, // °
        0x00B7 => 249, // ·
        0x00B2 => 253, // ²
        _ => b'?',
    }
}

/// Convert a CP437 byte to Unicode character
fn cp437_to_char(b: u8) -> char {
    // CP437 lookup table for 128-255
    const CP437_HIGH: [char; 128] = [
        'Ç', 'ü', 'é', 'â', 'ä', 'à', 'å', 'ç', 'ê', 'ë', 'è', 'ï', 'î', 'ì', 'Ä', 'Å', 'É', 'æ', 'Æ', 'ô', 'ö', 'ò',
        'û', 'ù', 'ÿ', 'Ö', 'Ü', '¢', '£', '¥', '₧', 'ƒ', 'á', 'í', 'ó', 'ú', 'ñ', 'Ñ', 'ª', 'º', '¿', '⌐', '¬', '½',
        '¼', '¡', '«', '»', '░', '▒', '▓', '│', '┤', '╡', '╢', '╖', '╕', '╣', '║', '╗', '╝', '╜', '╛', '┐', '└', '┴',
        '┬', '├', '─', '┼', '╞', '╟', '╚', '╔', '╩', '╦', '╠', '═', '╬', '╧', '╨', '╤', '╥', '╙', '╘', '╒', '╓', '╫',
        '╪', '┘', '┌', '█', '▄', '▌', '▐', '▀', 'α', 'ß', 'Γ', 'π', 'Σ', 'σ', 'µ', 'τ', 'Φ', 'Θ', 'Ω', 'δ', '∞', 'φ',
        'ε', '∩', '≡', '±', '≥', '≤', '⌠', '⌡', '÷', '≈', '°', '∙', '·', '√', 'ⁿ', '²', '■', ' ',
    ];

    if b < 32 {
        // Control characters - return as-is or map to space
        char::from_u32(b as u32).unwrap_or(' ')
    } else if b < 127 {
        // ASCII printable
        b as char
    } else if b == 127 {
        // DEL character
        '⌂'
    } else {
        // High characters (128-255)
        CP437_HIGH[(b - 128) as usize]
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mime_to_formats() {
        let formats = mime_to_rdp_formats(&["text/plain", "text/html"]);
        assert!(formats.iter().any(|f| f.id == CF_UNICODETEXT));
        assert!(formats.iter().any(|f| f.id == CF_HTML));
    }

    #[test]
    fn test_format_to_mime() {
        assert_eq!(rdp_format_to_mime(CF_UNICODETEXT), Some("text/plain;charset=utf-8"));
        assert_eq!(rdp_format_to_mime(CF_HTML), Some("text/html"));
        assert_eq!(rdp_format_to_mime(CF_PNG), Some("image/png"));
        assert_eq!(rdp_format_to_mime(CF_FILEGROUPDESCRIPTORW), Some("text/uri-list"));
        assert_eq!(rdp_format_to_mime(49430), Some("text/uri-list"));
        assert_eq!(rdp_format_to_mime(0xFFFF), None);
    }

    #[test]
    fn test_text_to_unicode() {
        let converter = FormatConverter::new();
        let result = converter.text_to_unicode("Hello").unwrap();

        // "Hello" in UTF-16LE + null terminator
        assert_eq!(
            result,
            vec![
                b'H', 0, b'e', 0, b'l', 0, b'l', 0, b'o', 0, // "Hello"
                0, 0 // null terminator
            ]
        );
    }

    #[test]
    fn test_unicode_to_text() {
        let converter = FormatConverter::new();
        let data = vec![b'H', 0, b'i', 0, 0, 0]; // "Hi" + null
        let result = converter.unicode_to_text(&data).unwrap();
        assert_eq!(result, "Hi");
    }

    #[test]
    fn test_html_roundtrip() {
        let converter = FormatConverter::new();
        let html = "<b>Hello</b>";

        let cf_html = converter.html_to_cf_html(html).unwrap();
        let recovered = converter.cf_html_to_html(&cf_html).unwrap();

        assert_eq!(recovered, html);
    }

    #[test]
    fn test_clipboard_format_builders() {
        let text = ClipboardFormat::unicode_text();
        assert_eq!(text.id, CF_UNICODETEXT);
        assert!(text.name.is_none());

        let html = ClipboardFormat::html();
        assert_eq!(html.id, CF_HTML);
        assert_eq!(html.name, Some("HTML Format".to_string()));
    }

    #[test]
    fn test_uri_list_to_hdrop() {
        let converter = FormatConverter::new();
        let uri_list = "file:///home/user/test.txt";

        let hdrop = converter.uri_list_to_hdrop(uri_list).unwrap();

        // Check DROPFILES header
        assert_eq!(hdrop[0..4], 20u32.to_le_bytes()); // pFiles
        assert_eq!(hdrop[16..20], 1u32.to_le_bytes()); // fWide = TRUE
    }

    #[test]
    fn test_hdrop_roundtrip() {
        let converter = FormatConverter::new();
        let original = "file:///home/user/test.txt";

        let hdrop = converter.uri_list_to_hdrop(original).unwrap();
        let recovered = converter.hdrop_to_uri_list(&hdrop).unwrap();

        assert_eq!(recovered, original);
    }

    #[test]
    fn test_text_to_ansi() {
        let converter = FormatConverter::new();
        let result = converter.text_to_ansi("Hello").unwrap();
        assert_eq!(result, vec![b'H', b'e', b'l', b'l', b'o', 0]);
    }

    #[test]
    fn test_ansi_to_text() {
        let converter = FormatConverter::new();
        let data = vec![b'H', b'i', 0];
        let result = converter.ansi_to_text(&data).unwrap();
        assert_eq!(result, "Hi");
    }

    #[test]
    fn test_ansi_roundtrip_special_chars() {
        let converter = FormatConverter::new();
        // Test Euro sign and em-dash (Windows-1252 specific)
        let text = "Price: \u{20AC}100 \u{2014} test";
        let ansi = converter.text_to_ansi(text).unwrap();
        let recovered = converter.ansi_to_text(&ansi).unwrap();
        assert_eq!(recovered, text);
    }

    #[test]
    fn test_text_to_oem() {
        let converter = FormatConverter::new();
        let result = converter.text_to_oem("Hello").unwrap();
        assert_eq!(result, vec![b'H', b'e', b'l', b'l', b'o', 0]);
    }

    #[test]
    fn test_oem_to_text() {
        let converter = FormatConverter::new();
        let data = vec![b'H', b'i', 0];
        let result = converter.oem_to_text(&data).unwrap();
        assert_eq!(result, "Hi");
    }

    #[test]
    fn test_synthesized_text_formats_announced() {
        // Verify that announcing text also announces synthesized formats
        let formats = mime_to_rdp_formats(&["text/plain"]);
        assert!(formats.iter().any(|f| f.id == CF_UNICODETEXT));
        assert!(formats.iter().any(|f| f.id == CF_TEXT));
        assert!(formats.iter().any(|f| f.id == CF_OEMTEXT));
    }

    // =========================================================================
    // RTF Tests
    // =========================================================================

    #[test]
    fn test_validate_rtf_valid() {
        let converter = FormatConverter::new();
        let rtf = b"{\\rtf1\\ansi Hello World}";
        let result = converter.validate_rtf(rtf);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), rtf.to_vec());
    }

    #[test]
    fn test_validate_rtf_invalid_header() {
        let converter = FormatConverter::new();
        let not_rtf = b"Hello World";
        let result = converter.validate_rtf(not_rtf);
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_rtf_unmatched_braces() {
        let converter = FormatConverter::new();
        let rtf = b"{\\rtf1\\ansi Hello {World";
        let result = converter.validate_rtf(rtf);
        assert!(result.is_err());
    }

    #[test]
    fn test_is_rtf() {
        let converter = FormatConverter::new();
        assert!(converter.is_rtf(b"{\\rtf1 test}"));
        assert!(!converter.is_rtf(b"plain text"));
    }

    #[test]
    fn test_text_to_rtf() {
        let converter = FormatConverter::new();
        let result = converter.text_to_rtf("Hello\nWorld").unwrap();
        let rtf_str = std::str::from_utf8(&result).unwrap();

        assert!(rtf_str.starts_with("{\\rtf1"));
        assert!(rtf_str.contains("Hello"));
        assert!(rtf_str.contains("\\par"));
        assert!(rtf_str.contains("World"));
        assert!(rtf_str.ends_with("}"));
    }

    #[test]
    fn test_text_to_rtf_escapes() {
        let converter = FormatConverter::new();
        let result = converter.text_to_rtf("Test {braces} and \\backslash").unwrap();
        let rtf_str = std::str::from_utf8(&result).unwrap();

        assert!(rtf_str.contains("\\{braces\\}"));
        assert!(rtf_str.contains("\\\\backslash"));
    }

    #[test]
    fn test_rtf_to_text_simple() {
        let converter = FormatConverter::new();
        let rtf = b"{\\rtf1\\ansi Hello World}";
        let result = converter.rtf_to_text(rtf).unwrap();
        assert_eq!(result.trim(), "Hello World");
    }

    #[test]
    fn test_rtf_to_text_with_formatting() {
        let converter = FormatConverter::new();
        let rtf = b"{\\rtf1\\ansi{\\b Bold} and {\\i italic}}";
        let result = converter.rtf_to_text(rtf).unwrap();
        assert!(result.contains("Bold"));
        assert!(result.contains("italic"));
    }

    #[test]
    fn test_rtf_to_text_with_paragraphs() {
        let converter = FormatConverter::new();
        let rtf = b"{\\rtf1\\ansi Line1\\par Line2}";
        let result = converter.rtf_to_text(rtf).unwrap();
        assert!(result.contains("Line1\nLine2"));
    }

    #[test]
    fn test_rtf_roundtrip() {
        let converter = FormatConverter::new();
        let original = "Hello World!\nSecond line.";

        let rtf = converter.text_to_rtf(original).unwrap();
        let recovered = converter.rtf_to_text(&rtf).unwrap();

        // Trim because RTF may have some whitespace differences
        assert_eq!(recovered.trim(), original);
    }

    #[test]
    fn test_rtf_format_announced() {
        let formats = mime_to_rdp_formats(&["text/rtf"]);
        assert!(formats.iter().any(|f| f.id == CF_RTF));
        assert!(formats
            .iter()
            .any(|f| f.name.as_ref().is_some_and(|n| n == "Rich Text Format")));
    }

    #[test]
    fn test_rtf_format_to_mime() {
        assert_eq!(rdp_format_to_mime(CF_RTF), Some("text/rtf"));
    }
}
