//! Convert a captured cursor bitmap into IronRDP's wire pointer format.
//!
//! MS-RDPBCGR 2.2.9.1.1.4.4 (Color Pointer Update, whose `xorMaskData` layout
//! the New Pointer Update reuses) requires bottom-up XOR mask scan lines.
//! `RGBAPointer::data` (despite the field name) is written straight to the
//! wire as that XOR mask, and IronRDP's own client-side decoder
//! (`ironrdp-graphics`) reads each 32bpp pixel as `[b, g, r, a]`, so the
//! output here must be BGRA regardless of the source format. Alpha is
//! straight (not premultiplied) on the wire; no AND mask is sent for the
//! 32bpp case.
//!
//! Shapes up to 384x384 are supported, MS-RDPBCGR 2.2.7.2.7's absolute
//! protocol maximum; the caller (`WirePointer::into_display_update`) picks
//! `RGBAPointer` (New Pointer Update, ceiling 96x96) or `LargePointer`
//! (Fast-Path Large Pointer Update, needed above that) based on actual
//! shape size. `ironrdp-server` itself enforces the client's negotiated
//! ceiling — 32x32/96x96/384x384 depending on what `LargePointerSupportFlags`
//! it advertised — dropping anything the client didn't agree to accept, so
//! this module doesn't need to know the client's capabilities to be correct;
//! it only needs to reject what the protocol can never carry at any ceiling.

use lamco_pipewire::{PixelFormat, ffi::VideoFormat, meta::CursorBitmap};

/// Absolute protocol maximum a pointer shape can ever be, regardless of
/// client capability (MS-RDPBCGR 2.2.7.2.7, `LARGE_POINTER_FLAG_384x384`).
const MAX_POINTER_DIMENSION: u32 = 384;

/// Ceiling for the New/Color Pointer Update (`RGBAPointer`) specifically,
/// MS-RDPBCGR 2.2.9.1.1.4.4: 96x96 with `LARGE_POINTER_FLAG_96x96`, 32x32
/// without. Shapes above this need the dedicated Large Pointer Update PDU
/// instead — `LARGE_POINTER_FLAG_96x96` alone does not enable that PDU, only
/// `LARGE_POINTER_FLAG_384x384` does, so routing through `RGBAPointer` for
/// anything this size or smaller is correct regardless of which of those two
/// flags (if either) the client actually negotiated.
const RGBA_POINTER_MAX_DIMENSION: u32 = 96;

/// Bytes per pixel on the wire (32bpp BGRA XOR mask, no AND mask).
const WIRE_BYTES_PER_PIXEL: usize = 4;

/// A converted cursor shape, ready to hand to `ironrdp_server::RGBAPointer`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WirePointer {
    pub width: u16,
    pub height: u16,
    pub hot_x: u16,
    pub hot_y: u16,
    /// Tightly packed, bottom-up... no: top-to-bottom in this struct's own
    /// row order (row 0 first); `into_rgba_pointer` is what flips it to the
    /// wire's bottom-up requirement. Kept top-down here so callers that only
    /// want to inspect the shape (tests) see a natural row order.
    pub data: Vec<u8>,
}

/// Why a cursor bitmap couldn't be converted. Every variant is a reason to
/// skip this particular shape update, not to fail the connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorWireError {
    /// Zero width or height.
    Empty,
    /// Exceeds `MAX_POINTER_DIMENSION` in either axis.
    TooLarge { width: u32, height: u32 },
    /// SPA format has no known 32bpp BGRA/RGBA mapping.
    UnsupportedFormat(u32),
    /// `pixels` is shorter than `stride.abs() * height` demands.
    Truncated,
}

impl std::fmt::Display for CursorWireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(f, "cursor bitmap has zero width or height"),
            Self::TooLarge { width, height } => {
                write!(
                    f,
                    "cursor bitmap {width}x{height} exceeds the {MAX_POINTER_DIMENSION}x{MAX_POINTER_DIMENSION} pointer ceiling"
                )
            }
            Self::UnsupportedFormat(fmt_id) => {
                write!(f, "unsupported cursor bitmap SPA format {fmt_id}")
            }
            Self::Truncated => write!(
                f,
                "cursor bitmap pixel buffer is shorter than its declared stride/height"
            ),
        }
    }
}

impl std::error::Error for CursorWireError {}

/// Convert a decoded PipeWire cursor bitmap to a BGRA wire-ready pointer
/// shape, with the compositor's hotspot mapped in.
///
/// `hotspot` is `CursorMeta::hotspot` as-is (signed per the SPA struct);
/// negative or out-of-bounds values are clamped into the bitmap.
pub fn convert_cursor_bitmap(
    bitmap: &CursorBitmap,
    hotspot: (i32, i32),
) -> Result<WirePointer, CursorWireError> {
    if bitmap.width == 0 || bitmap.height == 0 {
        return Err(CursorWireError::Empty);
    }
    if bitmap.width > MAX_POINTER_DIMENSION || bitmap.height > MAX_POINTER_DIMENSION {
        return Err(CursorWireError::TooLarge {
            width: bitmap.width,
            height: bitmap.height,
        });
    }

    let format = PixelFormat::from_spa(VideoFormat::from_raw(bitmap.format))
        .ok_or(CursorWireError::UnsupportedFormat(bitmap.format))?;
    let swap_red_blue = match format {
        PixelFormat::BGRA => false,
        PixelFormat::RGBA => true,
        _ => return Err(CursorWireError::UnsupportedFormat(bitmap.format)),
    };

    let width = bitmap.width as usize;
    let height = bitmap.height as usize;
    let src_row_stride = bitmap.stride.unsigned_abs() as usize;
    let src_row_bytes = width * WIRE_BYTES_PER_PIXEL;
    // Source is bottom-up when stride is negative; a top-down source needs
    // its rows reversed to match. `data` here is stored top-down (see the
    // `WirePointer::data` doc comment); the wire-format flip happens at
    // `RGBAPointer` construction, not here.
    let src_is_bottom_up = bitmap.stride < 0;

    if bitmap.pixels.len() < src_row_stride.saturating_mul(height) {
        return Err(CursorWireError::Truncated);
    }

    let mut data = vec![0u8; height * src_row_bytes];
    for dst_row in 0..height {
        let src_row = if src_is_bottom_up {
            height - 1 - dst_row
        } else {
            dst_row
        };
        let src_off = src_row * src_row_stride;
        let dst_off = dst_row * src_row_bytes;
        let src_pixels = &bitmap.pixels[src_off..src_off + src_row_bytes];
        let dst_pixels = &mut data[dst_off..dst_off + src_row_bytes];
        if swap_red_blue {
            for (src_px, dst_px) in src_pixels
                .chunks_exact(4)
                .zip(dst_pixels.chunks_exact_mut(4))
            {
                dst_px[0] = src_px[2]; // B <- R
                dst_px[1] = src_px[1]; // G <- G
                dst_px[2] = src_px[0]; // R <- B
                dst_px[3] = src_px[3]; // A <- A (straight, not premultiplied)
            }
        } else {
            dst_pixels.copy_from_slice(src_pixels);
        }
    }

    let hot_x = hotspot.0.clamp(0, bitmap.width as i32 - 1) as u16;
    let hot_y = hotspot.1.clamp(0, bitmap.height as i32 - 1) as u16;

    Ok(WirePointer {
        width: bitmap.width as u16,
        height: bitmap.height as u16,
        hot_x,
        hot_y,
        data,
    })
}

/// Which `DisplayUpdate` pointer variant a shape should be sent as.
///
/// `ironrdp-server` itself drops whichever of these the client didn't
/// negotiate capability for (see the module doc), so this only needs to
/// pick the smallest PDU that can structurally carry the shape.
#[derive(Debug, Clone)]
pub enum WireDisplayUpdate {
    /// New Pointer Update, MS-RDPBCGR 2.2.9.1.1.4.4/.5. Used for anything up
    /// to `RGBA_POINTER_MAX_DIMENSION` (96x96) — its own ceiling regardless
    /// of which Large Pointer flags, if any, the client negotiated.
    Rgba(ironrdp_server::RGBAPointer),
    /// Fast-Path Large Pointer Update, MS-RDPBCGR 2.2.9.1.2.1.11. Used for
    /// shapes above `RGBA_POINTER_MAX_DIMENSION`, up to the protocol's
    /// absolute 384x384 maximum.
    Large(ironrdp_server::LargePointer),
}

impl WirePointer {
    /// Row-flip `data` (stored top-down, see the field doc) to the wire's
    /// row order for the given client.
    ///
    /// `client_needs_top_down_rows` should be `false` for spec-compliant
    /// clients: this flips to the bottom-up order MS-RDPBCGR 2.2.9.1.1.4.4
    /// requires. Pass `true` for a client known to render the XOR mask rows
    /// flipped relative to that requirement — confirmed for the Android
    /// Microsoft RD Client (detected via its EGFX `AVC_DISABLED` capability
    /// negotiation; see `SharedHandlerState::needs_android_pointer_updates`).
    /// For that client, sending the spec-compliant bottom-up order renders
    /// upside down, so this skips the flip and sends `data`'s natural
    /// top-down order instead, which that client's rendering bug then
    /// displays right-side up.
    fn wire_rows(&self, client_needs_top_down_rows: bool) -> Vec<u8> {
        if client_needs_top_down_rows {
            return self.data.clone();
        }
        let row_bytes = self.width as usize * WIRE_BYTES_PER_PIXEL;
        let height = self.height as usize;
        let mut flipped = vec![0u8; self.data.len()];
        for row in 0..height {
            let src_off = row * row_bytes;
            let dst_row = height - 1 - row;
            let dst_off = dst_row * row_bytes;
            flipped[dst_off..dst_off + row_bytes]
                .copy_from_slice(&self.data[src_off..src_off + row_bytes]);
        }
        flipped
    }

    /// Build the `ironrdp_server::RGBAPointer` to send. See `wire_rows` for
    /// `client_needs_top_down_rows`. Callers that don't already know the
    /// shape fits the New Pointer Update's 96x96 ceiling should use
    /// `into_display_update` instead, which picks the right variant.
    pub fn into_rgba_pointer(
        self,
        cache_index: u16,
        client_needs_top_down_rows: bool,
    ) -> ironrdp_server::RGBAPointer {
        let data = self.wire_rows(client_needs_top_down_rows);
        ironrdp_server::RGBAPointer {
            cache_index,
            width: self.width,
            height: self.height,
            hot_x: self.hot_x,
            hot_y: self.hot_y,
            data,
        }
    }

    /// Build the `ironrdp_server::LargePointer` to send. Same row-flip rule
    /// as `into_rgba_pointer`.
    pub fn into_large_pointer(
        self,
        cache_index: u16,
        client_needs_top_down_rows: bool,
    ) -> ironrdp_server::LargePointer {
        let data = self.wire_rows(client_needs_top_down_rows);
        ironrdp_server::LargePointer {
            cache_index,
            width: self.width,
            height: self.height,
            hot_x: self.hot_x,
            hot_y: self.hot_y,
            data,
        }
    }

    /// Pick `RGBAPointer` or `LargePointer` based on actual shape size and
    /// build it. The right choice for almost every caller; use
    /// `into_rgba_pointer`/`into_large_pointer` directly only when the
    /// variant is already known some other way.
    pub fn into_display_update(
        self,
        cache_index: u16,
        client_needs_top_down_rows: bool,
    ) -> WireDisplayUpdate {
        if u32::from(self.width) <= RGBA_POINTER_MAX_DIMENSION
            && u32::from(self.height) <= RGBA_POINTER_MAX_DIMENSION
        {
            WireDisplayUpdate::Rgba(self.into_rgba_pointer(cache_index, client_needs_top_down_rows))
        } else {
            WireDisplayUpdate::Large(
                self.into_large_pointer(cache_index, client_needs_top_down_rows),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bgra_bitmap(
        width: u32,
        height: u32,
        bottom_up: bool,
        fill: impl Fn(u32, u32) -> [u8; 4],
    ) -> CursorBitmap {
        let stride = width as i32 * 4;
        let mut pixels = vec![0u8; (width * height * 4) as usize];
        for y in 0..height {
            let row = if bottom_up { height - 1 - y } else { y };
            for x in 0..width {
                let px = fill(x, y);
                let off = (row * width + x) as usize * 4;
                pixels[off..off + 4].copy_from_slice(&px);
            }
        }
        CursorBitmap {
            format: PixelFormat::BGRA.to_spa().as_raw(),
            width,
            height,
            stride: if bottom_up { -stride } else { stride },
            pixels,
        }
    }

    #[test]
    fn rejects_empty_bitmap() {
        let bitmap = bgra_bitmap(0, 4, false, |_, _| [0; 4]);
        assert_eq!(
            convert_cursor_bitmap(&bitmap, (0, 0)),
            Err(CursorWireError::Empty)
        );
    }

    #[test]
    fn accepts_bitmap_above_the_old_32x32_ceiling() {
        // 48x48 is within the New Pointer Update's 96x96 ceiling and must no
        // longer be rejected; ironrdp-server itself enforces the client's
        // negotiated capability, not this module.
        let bitmap = bgra_bitmap(48, 48, false, |_, _| [0; 4]);
        assert!(convert_cursor_bitmap(&bitmap, (0, 0)).is_ok());
    }

    #[test]
    fn accepts_bitmap_up_to_the_absolute_384_maximum() {
        let bitmap = bgra_bitmap(384, 384, false, |_, _| [0; 4]);
        assert!(convert_cursor_bitmap(&bitmap, (0, 0)).is_ok());
    }

    #[test]
    fn rejects_bitmap_above_the_absolute_384_maximum() {
        let bitmap = bgra_bitmap(400, 400, false, |_, _| [0; 4]);
        assert_eq!(
            convert_cursor_bitmap(&bitmap, (0, 0)),
            Err(CursorWireError::TooLarge {
                width: 400,
                height: 400
            })
        );
    }

    #[test]
    fn top_down_bgra_round_trips_pixel_at_origin() {
        let bitmap = bgra_bitmap(2, 2, false, |x, y| {
            if x == 0 && y == 0 {
                [10, 20, 30, 255]
            } else {
                [0, 0, 0, 0]
            }
        });
        let converted = convert_cursor_bitmap(&bitmap, (0, 0)).unwrap();
        // data is stored top-down: row 0 is the source's top row.
        assert_eq!(&converted.data[0..4], &[10, 20, 30, 255]);
    }

    #[test]
    fn bottom_up_source_is_normalized_to_top_down_storage() {
        let bitmap = bgra_bitmap(2, 2, true, |x, y| {
            if x == 0 && y == 0 {
                [10, 20, 30, 255]
            } else {
                [0, 0, 0, 0]
            }
        });
        let converted = convert_cursor_bitmap(&bitmap, (0, 0)).unwrap();
        assert_eq!(&converted.data[0..4], &[10, 20, 30, 255]);
    }

    #[test]
    fn rgba_source_is_swapped_to_bgra() {
        let stride = 2 * 4;
        let mut pixels = vec![0u8; 2 * 2 * 4];
        // Pixel (0,0) = R=10 G=20 B=30 A=255 in RGBA source order.
        pixels[0..4].copy_from_slice(&[10, 20, 30, 255]);
        let bitmap = CursorBitmap {
            format: PixelFormat::RGBA.to_spa().as_raw(),
            width: 2,
            height: 2,
            stride,
            pixels,
        };
        let converted = convert_cursor_bitmap(&bitmap, (0, 0)).unwrap();
        // Wire order is BGRA: expect B=30 G=20 R=10 A=255.
        assert_eq!(&converted.data[0..4], &[30, 20, 10, 255]);
    }

    #[test]
    fn hotspot_is_clamped_into_bounds() {
        let bitmap = bgra_bitmap(4, 4, false, |_, _| [0; 4]);
        let converted = convert_cursor_bitmap(&bitmap, (-5, 100)).unwrap();
        assert_eq!(converted.hot_x, 0);
        assert_eq!(converted.hot_y, 3);
    }

    #[test]
    fn into_rgba_pointer_flips_to_bottom_up_wire_order() {
        let bitmap = bgra_bitmap(2, 2, false, |x, y| {
            if x == 0 && y == 0 {
                [10, 20, 30, 255]
            } else {
                [1, 2, 3, 4]
            }
        });
        let converted = convert_cursor_bitmap(&bitmap, (0, 0)).unwrap();
        let pointer = converted.into_rgba_pointer(0, false);
        // Top row in storage (containing our marker pixel) must land in the
        // LAST row on the wire (bottom-up).
        let row_bytes = pointer.width as usize * 4;
        let last_row = &pointer.data[pointer.data.len() - row_bytes..];
        assert_eq!(&last_row[0..4], &[10, 20, 30, 255]);
    }

    #[test]
    fn into_rgba_pointer_keeps_top_down_rows_for_android_quirk() {
        let bitmap = bgra_bitmap(2, 2, false, |x, y| {
            if x == 0 && y == 0 {
                [10, 20, 30, 255]
            } else {
                [1, 2, 3, 4]
            }
        });
        let converted = convert_cursor_bitmap(&bitmap, (0, 0)).unwrap();
        let pointer = converted.into_rgba_pointer(0, true);
        // Marker pixel stays in the FIRST row: no flip for the Android quirk.
        assert_eq!(&pointer.data[0..4], &[10, 20, 30, 255]);
    }

    #[test]
    fn into_display_update_routes_96x96_and_below_through_rgba_pointer() {
        let bitmap = bgra_bitmap(96, 96, false, |_, _| [0; 4]);
        let converted = convert_cursor_bitmap(&bitmap, (0, 0)).unwrap();
        match converted.into_display_update(0, false) {
            WireDisplayUpdate::Rgba(_) => {}
            WireDisplayUpdate::Large(_) => panic!("96x96 must route through RGBAPointer"),
        }
    }

    #[test]
    fn into_display_update_routes_above_96x96_through_large_pointer() {
        let bitmap = bgra_bitmap(97, 97, false, |_, _| [0; 4]);
        let converted = convert_cursor_bitmap(&bitmap, (0, 0)).unwrap();
        match converted.into_display_update(0, false) {
            WireDisplayUpdate::Large(_) => {}
            WireDisplayUpdate::Rgba(_) => panic!("97x97 must route through LargePointer"),
        }
    }

    #[test]
    fn into_large_pointer_flips_to_bottom_up_wire_order() {
        let bitmap = bgra_bitmap(2, 2, false, |x, y| {
            if x == 0 && y == 0 {
                [10, 20, 30, 255]
            } else {
                [1, 2, 3, 4]
            }
        });
        let converted = convert_cursor_bitmap(&bitmap, (0, 0)).unwrap();
        let pointer = converted.into_large_pointer(0, false);
        let row_bytes = pointer.width as usize * 4;
        let last_row = &pointer.data[pointer.data.len() - row_bytes..];
        assert_eq!(&last_row[0..4], &[10, 20, 30, 255]);
    }
}
