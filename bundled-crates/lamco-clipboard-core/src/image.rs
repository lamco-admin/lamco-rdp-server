//! Image format conversion utilities.
//!
//! This module provides conversion between image formats commonly used in
//! clipboard operations, particularly the Windows DIB (Device Independent Bitmap)
//! format and standard image formats like PNG and JPEG.
//!
//! # Feature Flag
//!
//! This module requires the `image` feature:
//!
//! ```toml
//! [dependencies]
//! lamco-clipboard-core = { version = "0.1", features = ["image"] }
//! ```
//!
//! # Supported Conversions
//!
//! - PNG ↔ DIB (CF_DIB format 8, 40-byte header)
//! - PNG ↔ DIBV5 (CF_DIBV5 format 17, 124-byte header with alpha)
//! - JPEG ↔ DIB
//! - JPEG ↔ DIBV5
//! - BMP ↔ DIB
//! - GIF → PNG (read-only, converts to PNG for output)
//!
//! # DIB vs DIBV5
//!
//! - **DIB (CF_DIB)**: Standard Windows bitmap, 40-byte header, no alpha support
//! - **DIBV5 (CF_DIBV5)**: Extended bitmap, 124-byte header, full alpha and color space support
//!
//! Use DIBV5 for images with transparency. Modern Windows applications like
//! Paint.NET and screenshot tools use DIBV5 to preserve alpha channels.

use bytes::{BufMut, BytesMut};
use image::{DynamicImage, ImageFormat};

use crate::{ClipboardError, ClipboardResult};

/// Convert PNG image data to DIB (Device Independent Bitmap) format.
///
/// DIB is the standard Windows bitmap format used in clipboard operations.
/// This function decodes the PNG and creates a 32-bit BGRA DIB.
///
/// # Example
///
/// ```ignore
/// use lamco_clipboard_core::image::png_to_dib;
///
/// let png_data = std::fs::read("image.png")?;
/// let dib_data = png_to_dib(&png_data)?;
/// ```
pub fn png_to_dib(png_data: &[u8]) -> ClipboardResult<Vec<u8>> {
    let image = image::load_from_memory_with_format(png_data, ImageFormat::Png)
        .map_err(|e| ClipboardError::ImageDecode(e.to_string()))?;

    create_dib_from_image(&image)
}

/// Convert JPEG image data to DIB format.
pub fn jpeg_to_dib(jpeg_data: &[u8]) -> ClipboardResult<Vec<u8>> {
    let image = image::load_from_memory_with_format(jpeg_data, ImageFormat::Jpeg)
        .map_err(|e| ClipboardError::ImageDecode(e.to_string()))?;

    create_dib_from_image(&image)
}

/// Convert GIF image data to DIB format.
///
/// Note: GIF animations are not supported; only the first frame is converted.
pub fn gif_to_dib(gif_data: &[u8]) -> ClipboardResult<Vec<u8>> {
    let image = image::load_from_memory_with_format(gif_data, ImageFormat::Gif)
        .map_err(|e| ClipboardError::ImageDecode(e.to_string()))?;

    create_dib_from_image(&image)
}

/// Convert BMP file data to DIB format.
///
/// BMP files have a 14-byte file header followed by the DIB data.
/// This function extracts the DIB portion.
pub fn bmp_to_dib(bmp_data: &[u8]) -> ClipboardResult<Vec<u8>> {
    if bmp_data.len() < 14 {
        return Err(ClipboardError::ImageDecode("BMP file too small".to_string()));
    }

    // Verify BMP signature
    if &bmp_data[0..2] != b"BM" {
        return Err(ClipboardError::ImageDecode("Invalid BMP signature".to_string()));
    }

    // DIB is everything after the 14-byte file header
    Ok(bmp_data[14..].to_vec())
}

/// Convert DIB data to PNG format.
///
/// This is the most common conversion for clipboard images going from
/// Windows to Linux, as PNG is widely supported and lossless.
pub fn dib_to_png(dib_data: &[u8]) -> ClipboardResult<Vec<u8>> {
    let image = parse_dib_to_image(dib_data)?;

    let mut png_data = Vec::new();
    image
        .write_to(&mut std::io::Cursor::new(&mut png_data), ImageFormat::Png)
        .map_err(|e| ClipboardError::ImageEncode(e.to_string()))?;

    Ok(png_data)
}

/// Convert DIB data to JPEG format.
///
/// JPEG is lossy but produces smaller files. Use for photographs.
pub fn dib_to_jpeg(dib_data: &[u8]) -> ClipboardResult<Vec<u8>> {
    let image = parse_dib_to_image(dib_data)?;

    let mut jpeg_data = Vec::new();
    image
        .write_to(&mut std::io::Cursor::new(&mut jpeg_data), ImageFormat::Jpeg)
        .map_err(|e| ClipboardError::ImageEncode(e.to_string()))?;

    Ok(jpeg_data)
}

/// Convert DIB data to BMP file format.
///
/// This adds the 14-byte BMP file header to the DIB data.
pub fn dib_to_bmp(dib_data: &[u8]) -> ClipboardResult<Vec<u8>> {
    if dib_data.len() < 40 {
        return Err(ClipboardError::ImageDecode("DIB too small".to_string()));
    }

    // Parse DIB header to calculate file size
    let file_size =
        u32::try_from(14 + dib_data.len()).map_err(|_| ClipboardError::ImageDecode("DIB too large".to_string()))?;
    let pixel_offset: u32 = 14 + 40; // File header + DIB header (minimum)

    let mut bmp = BytesMut::new();

    // BMP file header (14 bytes)
    bmp.put_slice(b"BM"); // Signature
    bmp.put_u32_le(file_size); // File size
    bmp.put_u16_le(0); // Reserved1
    bmp.put_u16_le(0); // Reserved2
    bmp.put_u32_le(pixel_offset); // Pixel data offset

    // Append DIB data
    bmp.put_slice(dib_data);

    Ok(bmp.to_vec())
}

/// Convert any supported image format to DIB.
///
/// Automatically detects the input format based on magic bytes.
pub fn any_to_dib(data: &[u8]) -> ClipboardResult<Vec<u8>> {
    let image = image::load_from_memory(data).map_err(|e| ClipboardError::ImageDecode(e.to_string()))?;

    create_dib_from_image(&image)
}

// =============================================================================
// DIBV5 Functions (CF_DIBV5 format 17)
// =============================================================================

/// Convert PNG image data to DIBV5 format.
///
/// DIBV5 is the extended Windows bitmap format that supports alpha channels
/// and color space information. This creates an sRGB DIBV5 with a 124-byte
/// BITMAPV5HEADER.
///
/// # Example
///
/// ```ignore
/// use lamco_clipboard_core::image::png_to_dibv5;
///
/// let png_data = std::fs::read("transparent.png")?;
/// let dibv5_data = png_to_dibv5(&png_data)?;
/// ```
pub fn png_to_dibv5(png_data: &[u8]) -> ClipboardResult<Vec<u8>> {
    let image = image::load_from_memory_with_format(png_data, ImageFormat::Png)
        .map_err(|e| ClipboardError::ImageDecode(e.to_string()))?;

    create_dibv5_from_image(&image)
}

/// Convert JPEG image data to DIBV5 format.
///
/// Note: JPEG doesn't support transparency, so the alpha channel will be 255.
pub fn jpeg_to_dibv5(jpeg_data: &[u8]) -> ClipboardResult<Vec<u8>> {
    let image = image::load_from_memory_with_format(jpeg_data, ImageFormat::Jpeg)
        .map_err(|e| ClipboardError::ImageDecode(e.to_string()))?;

    create_dibv5_from_image(&image)
}

/// Convert DIBV5 data to PNG format.
///
/// This is the most common conversion for clipboard images going from
/// Windows to Linux. PNG preserves the alpha channel from DIBV5.
pub fn dibv5_to_png(dibv5_data: &[u8]) -> ClipboardResult<Vec<u8>> {
    let image = parse_dibv5_to_image(dibv5_data)?;

    let mut png_data = Vec::new();
    image
        .write_to(&mut std::io::Cursor::new(&mut png_data), ImageFormat::Png)
        .map_err(|e| ClipboardError::ImageEncode(e.to_string()))?;

    Ok(png_data)
}

/// Convert DIBV5 data to JPEG format.
///
/// Note: JPEG is lossy and doesn't support transparency.
/// Use `dibv5_to_png` to preserve alpha.
pub fn dibv5_to_jpeg(dibv5_data: &[u8]) -> ClipboardResult<Vec<u8>> {
    let image = parse_dibv5_to_image(dibv5_data)?;

    let mut jpeg_data = Vec::new();
    image
        .write_to(&mut std::io::Cursor::new(&mut jpeg_data), ImageFormat::Jpeg)
        .map_err(|e| ClipboardError::ImageEncode(e.to_string()))?;

    Ok(jpeg_data)
}

/// Convert any supported image format to DIBV5.
///
/// Automatically detects the input format based on magic bytes.
/// Use DIBV5 when transparency preservation is important.
pub fn any_to_dibv5(data: &[u8]) -> ClipboardResult<Vec<u8>> {
    let image = image::load_from_memory(data).map_err(|e| ClipboardError::ImageDecode(e.to_string()))?;

    create_dibv5_from_image(&image)
}

/// Check if image data has any transparent pixels.
///
/// Returns `true` if any pixel has alpha < 255.
/// Use this to decide whether to use DIB or DIBV5 format.
pub fn has_transparency(image_data: &[u8]) -> bool {
    if let Ok(img) = image::load_from_memory(image_data) {
        let rgba = img.to_rgba8();
        rgba.pixels().any(|p| p[3] != 255)
    } else {
        false
    }
}

/// Get image dimensions from DIB data without full decode.
///
/// Returns (width, height) in pixels.
pub fn dib_dimensions(dib_data: &[u8]) -> ClipboardResult<(u32, u32)> {
    if dib_data.len() < 12 {
        return Err(ClipboardError::ImageDecode("DIB too small".to_string()));
    }

    let width = i32::from_le_bytes([dib_data[4], dib_data[5], dib_data[6], dib_data[7]]).unsigned_abs();
    let height = i32::from_le_bytes([dib_data[8], dib_data[9], dib_data[10], dib_data[11]]).unsigned_abs();

    Ok((width, height))
}

// =============================================================================
// Internal Functions
// =============================================================================

/// Create DIB data from a DynamicImage.
fn create_dib_from_image(image: &DynamicImage) -> ClipboardResult<Vec<u8>> {
    let rgba = image.to_rgba8();
    let (width, height) = (rgba.width(), rgba.height());

    let mut dib = BytesMut::new();

    // BITMAPINFOHEADER structure (40 bytes)
    dib.put_u32_le(40); // biSize
    dib.put_i32_le(i32::try_from(width).unwrap_or(i32::MAX)); // biWidth
    dib.put_i32_le(-i32::try_from(height).unwrap_or(i32::MAX)); // biHeight (negative for top-down)
    dib.put_u16_le(1); // biPlanes
    dib.put_u16_le(32); // biBitCount (32 bits for BGRA)
    dib.put_u32_le(0); // biCompression (BI_RGB = 0)

    let image_size = width.saturating_mul(height).saturating_mul(4);
    dib.put_u32_le(image_size); // biSizeImage

    dib.put_i32_le(0); // biXPelsPerMeter
    dib.put_i32_le(0); // biYPelsPerMeter
    dib.put_u32_le(0); // biClrUsed
    dib.put_u32_le(0); // biClrImportant

    // Pixel data (convert RGBA to BGRA - Windows byte order)
    for pixel in rgba.pixels() {
        dib.put_u8(pixel[2]); // Blue
        dib.put_u8(pixel[1]); // Green
        dib.put_u8(pixel[0]); // Red
        dib.put_u8(pixel[3]); // Alpha
    }

    Ok(dib.to_vec())
}

/// Parse DIB data into a DynamicImage.
fn parse_dib_to_image(dib_data: &[u8]) -> ClipboardResult<DynamicImage> {
    if dib_data.len() < 40 {
        return Err(ClipboardError::ImageDecode("DIB too small".to_string()));
    }

    // Parse BITMAPINFOHEADER
    let bi_size = u32::from_le_bytes([dib_data[0], dib_data[1], dib_data[2], dib_data[3]]);
    if bi_size < 40 {
        return Err(ClipboardError::ImageDecode("Invalid DIB header size".to_string()));
    }

    let width = i32::from_le_bytes([dib_data[4], dib_data[5], dib_data[6], dib_data[7]]).unsigned_abs();
    let height_raw = i32::from_le_bytes([dib_data[8], dib_data[9], dib_data[10], dib_data[11]]);
    let height = height_raw.unsigned_abs();
    let top_down = height_raw < 0;
    let bit_count = u16::from_le_bytes([dib_data[14], dib_data[15]]);

    let header_size = bi_size as usize;
    if header_size >= dib_data.len() {
        return Err(ClipboardError::ImageDecode("DIB header larger than data".to_string()));
    }
    let pixel_data = &dib_data[header_size..];

    // Convert based on bit depth
    let image = match bit_count {
        32 => convert_32bit_dib(pixel_data, width, height, top_down)?,
        24 => convert_24bit_dib(pixel_data, width, height, top_down)?,
        _ => {
            return Err(ClipboardError::ImageDecode(format!(
                "Unsupported DIB bit depth: {}",
                bit_count
            )))
        }
    };

    Ok(image)
}

/// Convert 32-bit BGRA DIB to RGBA image.
fn convert_32bit_dib(pixel_data: &[u8], width: u32, height: u32, top_down: bool) -> ClipboardResult<DynamicImage> {
    let expected_size = (width as usize) * (height as usize) * 4;
    if pixel_data.len() < expected_size {
        return Err(ClipboardError::ImageDecode(format!(
            "Insufficient pixel data: {} < {}",
            pixel_data.len(),
            expected_size
        )));
    }

    let mut rgba_data = Vec::with_capacity(expected_size);

    for y in 0..height {
        let row_y = if top_down { y } else { height - 1 - y };
        let row_offset = (row_y as usize) * (width as usize) * 4;

        for x in 0..width {
            let pixel_offset = row_offset + (x as usize) * 4;
            if pixel_offset + 3 < pixel_data.len() {
                rgba_data.push(pixel_data[pixel_offset + 2]); // Red
                rgba_data.push(pixel_data[pixel_offset + 1]); // Green
                rgba_data.push(pixel_data[pixel_offset]); // Blue
                rgba_data.push(pixel_data[pixel_offset + 3]); // Alpha
            }
        }
    }

    image::RgbaImage::from_raw(width, height, rgba_data)
        .map(DynamicImage::ImageRgba8)
        .ok_or_else(|| ClipboardError::ImageDecode("Failed to create image from DIB".to_string()))
}

/// Convert 24-bit BGR DIB to RGB image.
fn convert_24bit_dib(pixel_data: &[u8], width: u32, height: u32, top_down: bool) -> ClipboardResult<DynamicImage> {
    // 24-bit DIB rows are aligned to 4-byte boundaries
    let row_size = (width * 3).div_ceil(4) * 4;
    let expected_size = (row_size as usize) * (height as usize);

    if pixel_data.len() < expected_size {
        return Err(ClipboardError::ImageDecode(format!(
            "Insufficient pixel data: {} < {}",
            pixel_data.len(),
            expected_size
        )));
    }

    let mut rgb_data = Vec::with_capacity((width as usize) * (height as usize) * 3);

    for y in 0..height {
        let row_y = if top_down { y } else { height - 1 - y };
        let row_offset = (row_y as usize) * (row_size as usize);

        for x in 0..width {
            let pixel_offset = row_offset + (x as usize) * 3;
            if pixel_offset + 2 < pixel_data.len() {
                rgb_data.push(pixel_data[pixel_offset + 2]); // Red
                rgb_data.push(pixel_data[pixel_offset + 1]); // Green
                rgb_data.push(pixel_data[pixel_offset]); // Blue
            }
        }
    }

    image::RgbImage::from_raw(width, height, rgb_data)
        .map(DynamicImage::ImageRgb8)
        .ok_or_else(|| ClipboardError::ImageDecode("Failed to create image from DIB".to_string()))
}

// =============================================================================
// DIBV5 Internal Functions
// =============================================================================

/// BITMAPV5HEADER size in bytes.
const DIBV5_HEADER_SIZE: usize = 124;

/// LCS_sRGB color space type ("sRGB" in little-endian ASCII).
const LCS_SRGB: u32 = 0x7352_4742;

/// LCS_GM_IMAGES rendering intent (perceptual).
const LCS_GM_IMAGES: u32 = 2;

/// Create DIBV5 data from a DynamicImage.
///
/// Creates a 124-byte BITMAPV5HEADER with:
/// - BI_BITFIELDS compression (masks for BGRA)
/// - sRGB color space
/// - Full alpha channel support
fn create_dibv5_from_image(image: &DynamicImage) -> ClipboardResult<Vec<u8>> {
    let rgba = image.to_rgba8();
    let (width, height) = (rgba.width(), rgba.height());

    // Pre-calculate sizes
    let image_size = width.saturating_mul(height).saturating_mul(4);
    let total_size = DIBV5_HEADER_SIZE + (image_size as usize);

    let mut dib = BytesMut::with_capacity(total_size);

    // BITMAPV5HEADER structure (124 bytes)
    // Offsets 0-3: Header size
    dib.put_u32_le(DIBV5_HEADER_SIZE as u32); // bV5Size

    // Offsets 4-7: Width
    dib.put_i32_le(i32::try_from(width).unwrap_or(i32::MAX)); // bV5Width

    // Offsets 8-11: Height (negative = top-down bitmap)
    dib.put_i32_le(-i32::try_from(height).unwrap_or(i32::MAX)); // bV5Height

    // Offsets 12-13: Planes (always 1)
    dib.put_u16_le(1); // bV5Planes

    // Offsets 14-15: Bit count (32-bit BGRA)
    dib.put_u16_le(32); // bV5BitCount

    // Offsets 16-19: Compression (BI_BITFIELDS = 3)
    dib.put_u32_le(3); // bV5Compression

    // Offsets 20-23: Image size
    dib.put_u32_le(image_size); // bV5SizeImage

    // Offsets 24-27: X pixels per meter (0 = undefined)
    dib.put_i32_le(0); // bV5XPelsPerMeter

    // Offsets 28-31: Y pixels per meter (0 = undefined)
    dib.put_i32_le(0); // bV5YPelsPerMeter

    // Offsets 32-35: Colors used (0 = all)
    dib.put_u32_le(0); // bV5ClrUsed

    // Offsets 36-39: Colors important (0 = all)
    dib.put_u32_le(0); // bV5ClrImportant

    // Offsets 40-43: Red channel mask (byte 2 in BGRA)
    dib.put_u32_le(0x00FF_0000); // bV5RedMask

    // Offsets 44-47: Green channel mask (byte 1 in BGRA)
    dib.put_u32_le(0x0000_FF00); // bV5GreenMask

    // Offsets 48-51: Blue channel mask (byte 0 in BGRA)
    dib.put_u32_le(0x0000_00FF); // bV5BlueMask

    // Offsets 52-55: Alpha channel mask (byte 3 in BGRA)
    dib.put_u32_le(0xFF00_0000); // bV5AlphaMask

    // Offsets 56-59: Color space type (sRGB)
    dib.put_u32_le(LCS_SRGB); // bV5CSType

    // Offsets 60-95: CIEXYZTRIPLE endpoints (36 bytes, zeros for sRGB)
    for _ in 0..9 {
        dib.put_u32_le(0);
    }

    // Offsets 96-99: Gamma red (0 = use sRGB default)
    dib.put_u32_le(0); // bV5GammaRed

    // Offsets 100-103: Gamma green
    dib.put_u32_le(0); // bV5GammaGreen

    // Offsets 104-107: Gamma blue
    dib.put_u32_le(0); // bV5GammaBlue

    // Offsets 108-111: Rendering intent
    dib.put_u32_le(LCS_GM_IMAGES); // bV5Intent

    // Offsets 112-115: ICC profile data offset (0 = none)
    dib.put_u32_le(0); // bV5ProfileData

    // Offsets 116-119: ICC profile size (0 = none)
    dib.put_u32_le(0); // bV5ProfileSize

    // Offsets 120-123: Reserved
    dib.put_u32_le(0); // bV5Reserved

    debug_assert_eq!(dib.len(), DIBV5_HEADER_SIZE);

    // Pixel data: convert RGBA to BGRA (Windows byte order)
    for pixel in rgba.pixels() {
        dib.put_u8(pixel[2]); // Blue
        dib.put_u8(pixel[1]); // Green
        dib.put_u8(pixel[0]); // Red
        dib.put_u8(pixel[3]); // Alpha
    }

    Ok(dib.to_vec())
}

/// Parse DIBV5 data into a DynamicImage.
///
/// Handles both standard 124-byte DIBV5 headers and the "short DIBV5" bug
/// where some applications use a 40-byte header with format ID 17.
fn parse_dibv5_to_image(dibv5_data: &[u8]) -> ClipboardResult<DynamicImage> {
    if dibv5_data.len() < 4 {
        return Err(ClipboardError::ImageDecode("DIBV5 too small".to_string()));
    }

    // Read header size to determine format variant
    let header_size = u32::from_le_bytes([dibv5_data[0], dibv5_data[1], dibv5_data[2], dibv5_data[3]]);

    match header_size {
        40 => {
            // "Short DIBV5" - some apps use CF_DIBV5 format ID but DIB header
            // Fall back to regular DIB parser
            parse_dib_to_image(dibv5_data)
        }
        124 => {
            // Standard DIBV5 with full 124-byte header
            parse_full_dibv5(dibv5_data)
        }
        _ => Err(ClipboardError::ImageDecode(format!(
            "Invalid DIBV5 header size: {} (expected 40 or 124)",
            header_size
        ))),
    }
}

/// Parse standard 124-byte DIBV5 data.
fn parse_full_dibv5(data: &[u8]) -> ClipboardResult<DynamicImage> {
    if data.len() < DIBV5_HEADER_SIZE {
        return Err(ClipboardError::ImageDecode(
            "DIBV5 data too small for header".to_string(),
        ));
    }

    // Parse dimensions
    let width = i32::from_le_bytes([data[4], data[5], data[6], data[7]]).unsigned_abs();
    let height_raw = i32::from_le_bytes([data[8], data[9], data[10], data[11]]);
    let height = height_raw.unsigned_abs();
    let top_down = height_raw < 0;

    // Parse bit depth and compression
    let bit_count = u16::from_le_bytes([data[14], data[15]]);
    let compression = u32::from_le_bytes([data[16], data[17], data[18], data[19]]);

    // Parse color masks for BI_BITFIELDS (compression == 3)
    let (red_mask, green_mask, blue_mask, alpha_mask) = if compression == 3 {
        (
            u32::from_le_bytes([data[40], data[41], data[42], data[43]]),
            u32::from_le_bytes([data[44], data[45], data[46], data[47]]),
            u32::from_le_bytes([data[48], data[49], data[50], data[51]]),
            u32::from_le_bytes([data[52], data[53], data[54], data[55]]),
        )
    } else {
        // Default BGRA masks for BI_RGB
        (0x00FF_0000, 0x0000_FF00, 0x0000_00FF, 0xFF00_0000)
    };

    // Pixel data starts after 124-byte header
    let pixel_data = &data[DIBV5_HEADER_SIZE..];

    match bit_count {
        32 => convert_32bit_dibv5(
            pixel_data, width, height, top_down, red_mask, green_mask, blue_mask, alpha_mask,
        ),
        24 => convert_24bit_dib(pixel_data, width, height, top_down),
        _ => Err(ClipboardError::ImageDecode(format!(
            "Unsupported DIBV5 bit depth: {}",
            bit_count
        ))),
    }
}

/// Convert 32-bit DIBV5 pixel data using color masks.
#[allow(clippy::too_many_arguments)]
fn convert_32bit_dibv5(
    pixel_data: &[u8],
    width: u32,
    height: u32,
    top_down: bool,
    red_mask: u32,
    green_mask: u32,
    blue_mask: u32,
    alpha_mask: u32,
) -> ClipboardResult<DynamicImage> {
    let expected_size = (width as usize) * (height as usize) * 4;
    if pixel_data.len() < expected_size {
        return Err(ClipboardError::ImageDecode(format!(
            "Insufficient DIBV5 pixel data: {} < {}",
            pixel_data.len(),
            expected_size
        )));
    }

    // Calculate shifts for each color channel
    let red_shift = red_mask.trailing_zeros();
    let green_shift = green_mask.trailing_zeros();
    let blue_shift = blue_mask.trailing_zeros();
    let alpha_shift = alpha_mask.trailing_zeros();

    let mut rgba_data = Vec::with_capacity(expected_size);

    for y in 0..height {
        let row_y = if top_down { y } else { height - 1 - y };
        let row_offset = (row_y as usize) * (width as usize) * 4;

        for x in 0..width {
            let pixel_offset = row_offset + (x as usize) * 4;
            if pixel_offset + 3 < pixel_data.len() {
                let pixel = u32::from_le_bytes([
                    pixel_data[pixel_offset],
                    pixel_data[pixel_offset + 1],
                    pixel_data[pixel_offset + 2],
                    pixel_data[pixel_offset + 3],
                ]);

                // Extract channels using masks and shifts
                let red = ((pixel & red_mask) >> red_shift) as u8;
                let green = ((pixel & green_mask) >> green_shift) as u8;
                let blue = ((pixel & blue_mask) >> blue_shift) as u8;
                let alpha = if alpha_mask != 0 {
                    ((pixel & alpha_mask) >> alpha_shift) as u8
                } else {
                    255 // No alpha channel, assume opaque
                };

                rgba_data.push(red);
                rgba_data.push(green);
                rgba_data.push(blue);
                rgba_data.push(alpha);
            }
        }
    }

    image::RgbaImage::from_raw(width, height, rgba_data)
        .map(DynamicImage::ImageRgba8)
        .ok_or_else(|| ClipboardError::ImageDecode("Failed to create image from DIBV5".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_and_parse_dib() {
        // Create a small test image (10x10 red square)
        let image = DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(10, 10, image::Rgba([255, 0, 0, 255])));

        // Convert to DIB
        let dib = create_dib_from_image(&image).unwrap();

        // Verify DIB header
        assert!(dib.len() >= 40);
        assert_eq!(u32::from_le_bytes([dib[0], dib[1], dib[2], dib[3]]), 40); // biSize

        // Convert back to image
        let parsed = parse_dib_to_image(&dib).unwrap();
        assert_eq!(parsed.width(), 10);
        assert_eq!(parsed.height(), 10);
    }

    #[test]
    fn test_dib_dimensions() {
        let image = DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(100, 50, image::Rgba([0, 0, 0, 255])));

        let dib = create_dib_from_image(&image).unwrap();
        let (width, height) = dib_dimensions(&dib).unwrap();

        assert_eq!(width, 100);
        assert_eq!(height, 50);
    }

    #[test]
    fn test_png_roundtrip() {
        // Create a small PNG
        let image = DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(5, 5, image::Rgba([100, 150, 200, 255])));

        let mut png_data = Vec::new();
        image
            .write_to(&mut std::io::Cursor::new(&mut png_data), ImageFormat::Png)
            .unwrap();

        // PNG → DIB → PNG
        let dib = png_to_dib(&png_data).unwrap();
        let png_back = dib_to_png(&dib).unwrap();

        // Load and verify
        let loaded = image::load_from_memory(&png_back).unwrap();
        assert_eq!(loaded.width(), 5);
        assert_eq!(loaded.height(), 5);
    }

    #[test]
    fn test_bmp_roundtrip() {
        let image = DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(8, 8, image::Rgba([50, 100, 150, 255])));

        let dib = create_dib_from_image(&image).unwrap();

        // DIB → BMP → DIB
        let bmp = dib_to_bmp(&dib).unwrap();

        // Verify BMP signature
        assert_eq!(&bmp[0..2], b"BM");

        // Extract DIB back
        let dib_back = bmp_to_dib(&bmp).unwrap();
        assert_eq!(dib, dib_back);
    }

    #[test]
    fn test_invalid_dib() {
        // Too small
        assert!(parse_dib_to_image(&[0; 30]).is_err());

        // Invalid header size
        let mut invalid_dib = vec![0; 50];
        invalid_dib[0] = 10; // Invalid biSize < 40
        assert!(parse_dib_to_image(&invalid_dib).is_err());
    }

    #[test]
    fn test_invalid_bmp() {
        // Too small
        assert!(bmp_to_dib(&[0; 10]).is_err());

        // Invalid signature
        let mut invalid_bmp = vec![0; 20];
        invalid_bmp[0] = b'X';
        invalid_bmp[1] = b'Y';
        assert!(bmp_to_dib(&invalid_bmp).is_err());
    }

    // =========================================================================
    // DIBV5 Tests
    // =========================================================================

    #[test]
    fn test_create_and_parse_dibv5() {
        // Create a small test image (10x10 red with 50% transparency)
        let image = DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(10, 10, image::Rgba([255, 0, 0, 128])));

        // Convert to DIBV5
        let dibv5 = create_dibv5_from_image(&image).unwrap();

        // Verify DIBV5 header
        assert!(dibv5.len() >= 124);
        assert_eq!(u32::from_le_bytes([dibv5[0], dibv5[1], dibv5[2], dibv5[3]]), 124); // bV5Size

        // Convert back to image
        let parsed = parse_dibv5_to_image(&dibv5).unwrap();
        assert_eq!(parsed.width(), 10);
        assert_eq!(parsed.height(), 10);

        // Verify alpha channel preserved
        let rgba = parsed.to_rgba8();
        let pixel = rgba.get_pixel(0, 0);
        assert_eq!(pixel[3], 128); // Alpha should be preserved
    }

    #[test]
    fn test_dibv5_header_structure() {
        let image = DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(4, 4, image::Rgba([100, 150, 200, 128])));

        let dibv5 = create_dibv5_from_image(&image).unwrap();

        // Verify header fields
        assert_eq!(u32::from_le_bytes([dibv5[0], dibv5[1], dibv5[2], dibv5[3]]), 124); // Size
        assert_eq!(i32::from_le_bytes([dibv5[4], dibv5[5], dibv5[6], dibv5[7]]), 4); // Width
        assert_eq!(i32::from_le_bytes([dibv5[8], dibv5[9], dibv5[10], dibv5[11]]), -4); // Height (negative = top-down)
        assert_eq!(u16::from_le_bytes([dibv5[14], dibv5[15]]), 32); // Bit count
        assert_eq!(u32::from_le_bytes([dibv5[16], dibv5[17], dibv5[18], dibv5[19]]), 3); // BI_BITFIELDS

        // Color masks
        assert_eq!(
            u32::from_le_bytes([dibv5[40], dibv5[41], dibv5[42], dibv5[43]]),
            0x00FF0000
        ); // Red
        assert_eq!(
            u32::from_le_bytes([dibv5[44], dibv5[45], dibv5[46], dibv5[47]]),
            0x0000FF00
        ); // Green
        assert_eq!(
            u32::from_le_bytes([dibv5[48], dibv5[49], dibv5[50], dibv5[51]]),
            0x000000FF
        ); // Blue
        assert_eq!(
            u32::from_le_bytes([dibv5[52], dibv5[53], dibv5[54], dibv5[55]]),
            0xFF000000
        ); // Alpha

        // Color space
        assert_eq!(
            u32::from_le_bytes([dibv5[56], dibv5[57], dibv5[58], dibv5[59]]),
            LCS_SRGB
        );
    }

    #[test]
    fn test_png_to_dibv5_roundtrip() {
        // Create PNG with transparency
        let image = DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(5, 5, image::Rgba([50, 100, 150, 100])));

        let mut png_data = Vec::new();
        image
            .write_to(&mut std::io::Cursor::new(&mut png_data), ImageFormat::Png)
            .unwrap();

        // PNG → DIBV5 → PNG
        let dibv5 = png_to_dibv5(&png_data).unwrap();
        let png_back = dibv5_to_png(&dibv5).unwrap();

        // Load and verify
        let loaded = image::load_from_memory(&png_back).unwrap();
        assert_eq!(loaded.width(), 5);
        assert_eq!(loaded.height(), 5);

        // Verify alpha preserved
        let rgba = loaded.to_rgba8();
        let pixel = rgba.get_pixel(0, 0);
        assert_eq!(pixel[3], 100);
    }

    #[test]
    fn test_has_transparency() {
        // Image with transparency
        let transparent = DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(2, 2, image::Rgba([255, 0, 0, 128])));

        let mut transparent_png = Vec::new();
        transparent
            .write_to(&mut std::io::Cursor::new(&mut transparent_png), ImageFormat::Png)
            .unwrap();

        assert!(has_transparency(&transparent_png));

        // Opaque image
        let opaque = DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(2, 2, image::Rgba([255, 0, 0, 255])));

        let mut opaque_png = Vec::new();
        opaque
            .write_to(&mut std::io::Cursor::new(&mut opaque_png), ImageFormat::Png)
            .unwrap();

        assert!(!has_transparency(&opaque_png));
    }

    #[test]
    fn test_short_dibv5_fallback() {
        // Create a "short DIBV5" (40-byte header with format 17)
        // This tests the compatibility fallback
        let image = DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(3, 3, image::Rgba([255, 128, 64, 255])));

        // Create DIB (40-byte header)
        let dib = create_dib_from_image(&image).unwrap();
        assert_eq!(u32::from_le_bytes([dib[0], dib[1], dib[2], dib[3]]), 40);

        // Parse as DIBV5 should fall back to DIB parser
        let parsed = parse_dibv5_to_image(&dib).unwrap();
        assert_eq!(parsed.width(), 3);
        assert_eq!(parsed.height(), 3);
    }

    #[test]
    fn test_invalid_dibv5() {
        // Too small
        assert!(parse_dibv5_to_image(&[0; 3]).is_err());

        // Invalid header size
        let mut invalid = vec![0; 150];
        invalid[0] = 50; // Invalid size (not 40 or 124)
        assert!(parse_dibv5_to_image(&invalid).is_err());
    }

    #[test]
    fn test_dibv5_pixel_colors() {
        // Create image with specific colors
        let mut img = image::RgbaImage::new(2, 2);
        img.put_pixel(0, 0, image::Rgba([255, 0, 0, 255])); // Red
        img.put_pixel(1, 0, image::Rgba([0, 255, 0, 128])); // Green semi-transparent
        img.put_pixel(0, 1, image::Rgba([0, 0, 255, 64])); // Blue mostly transparent
        img.put_pixel(1, 1, image::Rgba([128, 128, 128, 0])); // Gray fully transparent

        let image = DynamicImage::ImageRgba8(img);

        // Round-trip through DIBV5
        let dibv5 = create_dibv5_from_image(&image).unwrap();
        let parsed = parse_dibv5_to_image(&dibv5).unwrap();
        let rgba = parsed.to_rgba8();

        // Verify colors
        assert_eq!(rgba.get_pixel(0, 0), &image::Rgba([255, 0, 0, 255]));
        assert_eq!(rgba.get_pixel(1, 0), &image::Rgba([0, 255, 0, 128]));
        assert_eq!(rgba.get_pixel(0, 1), &image::Rgba([0, 0, 255, 64]));
        assert_eq!(rgba.get_pixel(1, 1), &image::Rgba([128, 128, 128, 0]));
    }
}
