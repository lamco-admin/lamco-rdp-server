//! YUV444 to dual YUV420 packing for AVC444
//!
//! Implements the MS-RDPEGFX macroblock-level packing algorithm for encoding
//! YUV444 (4:4:4 chroma) content using two standard YUV420 H.264 streams.
//!
//! # The Core Algorithm
//!
//! AVC444 achieves full 4:4:4 chroma by splitting YUV444 into two YUV420 streams:
//!
//! 1. **Main View (Stream 1)**: Standard YUV420 encoding
//!    - Y plane: Full luma (unchanged)
//!    - U plane: 2×2 box filter subsampled from U444
//!    - V plane: 2×2 box filter subsampled from V444
//!
//! 2. **Auxiliary View (Stream 2)**: Residual chroma as "fake" luma
//!    - Y plane: Missing U444 samples (the 75% discarded by 4:2:0)
//!    - U plane: Missing V444 samples
//!    - V plane: Neutral (128) for encoder stability
//!
//! # Client Reconstruction
//!
//! The client combines both streams to reconstruct full YUV444:
//! - Even pixel positions: Use main view chroma (subsampled)
//! - Odd pixel positions: Use auxiliary view "luma" as chroma
//!
//! # MS-RDPEGFX Reference
//!
//! See MS-RDPEGFX Section 3.3.8.3.2 and Figure 7 for the specification.

use super::color_convert::{subsample_chroma_420, ColorMatrix, Yuv444Frame};

/// YUV420 frame (4:2:0 chroma subsampling)
///
/// Standard H.264-compatible frame format where chroma planes are
/// half the resolution of luma in both dimensions.
#[derive(Debug, Clone)]
pub struct Yuv420Frame {
    /// Luma plane (width × height)
    pub y: Vec<u8>,
    /// Chroma U (Cb) plane (width/2 × height/2)
    pub u: Vec<u8>,
    /// Chroma V (Cr) plane (width/2 × height/2)
    pub v: Vec<u8>,
    /// Frame width in pixels
    pub width: usize,
    /// Frame height in pixels
    pub height: usize,
}

impl Yuv420Frame {
    pub fn new(width: usize, height: usize) -> Self {
        let y_size = width * height;
        let uv_size = (width / 2) * (height / 2);
        Self {
            y: vec![0u8; y_size],
            u: vec![128u8; uv_size],
            v: vec![128u8; uv_size],
            width,
            height,
        }
    }

    #[inline]
    pub fn total_size(&self) -> usize {
        self.y.len() + self.u.len() + self.v.len()
    }

    /// Convert YUV420 back to BGRA for feeding to OpenH264
    ///
    /// OpenH264's `YUVBuffer::from_rgb_source()` expects BGRA input and does
    /// its own YUV420 conversion internally. Since we've already computed
    /// YUV420, we must convert back to BGRA.
    ///
    /// Uses BT.709 inverse matrix for HD content.
    pub fn to_bgra(&self) -> Vec<u8> {
        self.to_bgra_with_matrix(ColorMatrix::BT709)
    }

    /// Convert YUV420 back to BGRA with specified color matrix
    pub fn to_bgra_with_matrix(&self, matrix: ColorMatrix) -> Vec<u8> {
        let pixel_count = self.width * self.height;
        let mut bgra = vec![0u8; pixel_count * 4];

        // Get inverse matrix coefficients
        // Note: OpenH264 uses BT.601 matrix with limited range, so same inverse coefficients
        let (rv, gu, gv, bu) = match matrix {
            ColorMatrix::BT601 | ColorMatrix::OpenH264 => (1.402, -0.344136, -0.714136, 1.772),
            ColorMatrix::BT709 => (1.5748, -0.1873, -0.4681, 1.8556),
        };

        let chroma_width = self.width / 2;

        for y in 0..self.height {
            for x in 0..self.width {
                let y_val = self.y[y * self.width + x] as f32;

                // Chroma is subsampled - same value for 2×2 luma block
                let chroma_idx = (y / 2) * chroma_width + (x / 2);
                let u_val = self.u[chroma_idx] as f32 - 128.0;
                let v_val = self.v[chroma_idx] as f32 - 128.0;

                // YUV to RGB conversion
                let r = (y_val + rv * v_val).clamp(0.0, 255.0);
                let g = (y_val + gu * u_val + gv * v_val).clamp(0.0, 255.0);
                let b = (y_val + bu * u_val).clamp(0.0, 255.0);

                let idx = (y * self.width + x) * 4;
                bgra[idx] = b as u8; // B
                bgra[idx + 1] = g as u8; // G
                bgra[idx + 2] = r as u8; // R
                bgra[idx + 3] = 255; // A (opaque)
            }
        }

        bgra
    }

    /// Create YUV buffer compatible with OpenH264 encoding
    ///
    /// Returns the planes in I420 order: Y, U, V with correct strides.
    pub fn to_i420_planes(&self) -> (&[u8], &[u8], &[u8], usize, usize) {
        let y_stride = self.width;
        let uv_stride = self.width / 2;
        (&self.y, &self.u, &self.v, y_stride, uv_stride)
    }

    // === OpenH264 YUVSource-compatible interface ===
    // These methods match the openh264::formats::YUVSource trait signature,
    // allowing direct use with YUVSlices without double-conversion.

    #[inline]
    pub fn dimensions(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    /// For planar YUV420, Y stride = width, U/V stride = width/2
    #[inline]
    pub fn strides(&self) -> (usize, usize, usize) {
        (self.width, self.width / 2, self.width / 2)
    }

    #[inline]
    pub fn y_plane(&self) -> &[u8] {
        &self.y
    }

    #[inline]
    pub fn u_plane(&self) -> &[u8] {
        &self.u
    }

    #[inline]
    pub fn v_plane(&self) -> &[u8] {
        &self.v
    }

    /// Checks that:
    /// - Dimensions are even (required for YUV420)
    /// - Plane sizes match expected dimensions
    /// - Strides are valid
    pub fn validate_for_encoding(&self) -> Result<(), String> {
        if self.width % 2 != 0 {
            return Err(format!("Width {} must be even for YUV420", self.width));
        }
        if self.height % 2 != 0 {
            return Err(format!("Height {} must be even for YUV420", self.height));
        }

        let expected_y = self.width * self.height;
        if self.y.len() != expected_y {
            return Err(format!(
                "Y plane size {} doesn't match expected {}",
                self.y.len(),
                expected_y
            ));
        }

        let expected_uv = (self.width / 2) * (self.height / 2);
        if self.u.len() != expected_uv {
            return Err(format!(
                "U plane size {} doesn't match expected {}",
                self.u.len(),
                expected_uv
            ));
        }
        if self.v.len() != expected_uv {
            return Err(format!(
                "V plane size {} doesn't match expected {}",
                self.v.len(),
                expected_uv
            ));
        }

        Ok(())
    }
}

/// Create main YUV420 view (full luma + subsampled chroma)
///
/// This is the "luma view" or "Stream 1" in AVC444 terminology.
///
/// # Algorithm
///
/// - **Y plane**: Direct copy from YUV444 Y (no modification)
/// - **U plane**: 2×2 box filter subsample from YUV444 U
/// - **V plane**: 2×2 box filter subsample from YUV444 V
///
pub fn pack_main_view(yuv444: &Yuv444Frame) -> Yuv420Frame {
    let width = yuv444.width;
    let height = yuv444.height;

    // Y plane: Copy full luma (no subsampling)
    let y = yuv444.y.clone();

    // U plane: 2×2 box filter subsample
    let mut u = subsample_chroma_420(&yuv444.u, width, height);

    // V plane: 2×2 box filter subsample
    let v = subsample_chroma_420(&yuv444.v, width, height);

    // NOTE: We don't pad chroma planes here because openh264 does its own
    // macroblock alignment internally. Padding breaks openh264-rs buffer validation.

    // DEEP DIAGNOSTIC: Sample multiple screen positions to capture colorful areas
    use tracing::debug;
    if width == 1280 && height == 800 {
        debug!("═══ MAIN VIEW MULTI-POSITION ANALYSIS ═══");

        // Sample 5 strategic positions: top-left, top-right, center, bottom-left, bottom-right
        let sample_positions = [
            (0, 0, "Top-Left (0,0)"),
            (width - 4, 0, "Top-Right"),
            (width / 2, height / 2, "Center"),
            (0, height - 4, "Bottom-Left"),
            (width - 4, height - 4, "Bottom-Right"),
        ];

        for (x, y, label) in sample_positions {
            let idx = y * width + x;
            debug!("📍 {} @ ({}, {})", label, x, y);

            // Sample 2x2 block for analysis
            debug!(
                "  Y444: [{:3},{:3}] [{:3},{:3}]",
                yuv444.y[idx],
                yuv444.y[idx + 1],
                yuv444.y[idx + width],
                yuv444.y[idx + width + 1]
            );
            debug!(
                "  U444: [{:3},{:3}] [{:3},{:3}]",
                yuv444.u[idx],
                yuv444.u[idx + 1],
                yuv444.u[idx + width],
                yuv444.u[idx + width + 1]
            );
            debug!(
                "  V444: [{:3},{:3}] [{:3},{:3}]",
                yuv444.v[idx],
                yuv444.v[idx + 1],
                yuv444.v[idx + width],
                yuv444.v[idx + width + 1]
            );

            // Show subsampled result for this position
            let chroma_x = x / 2;
            let chroma_y = y / 2;
            let chroma_idx = chroma_y * (width / 2) + chroma_x;
            debug!(
                "  Main U420: {:3}, Main V420: {:3}",
                u[chroma_idx], v[chroma_idx]
            );
        }

        debug!("═══════════════════════════════════════════");
    }

    Yuv420Frame {
        y,
        u,
        v,
        width,
        height,
    }
}

/// Create auxiliary YUV420 view (residual chroma data)
///
/// This is the "chroma view" or "Stream 2" in AVC444 terminology.
///
/// # The Core Trick
///
/// Standard 4:2:0 subsampling keeps only 25% of chroma samples (one per 2×2 block).
/// The auxiliary view captures the other 75% by encoding chroma as "fake luma".
///
/// # Spec-Compliant Packing (MS-RDPEGFX Section 3.3.8.3.2)
///
/// For each pixel position (x, y):
/// - **Even positions** (x % 2 == 0 AND y % 2 == 0): Already in main view
/// - **Odd positions** (any other case): Packed into auxiliary view
///
/// The auxiliary frame structure:
/// - **Y plane**: U444 samples at odd positions
/// - **U plane**: V444 samples at odd positions (subsampled)
/// - **V plane**: Neutral (128) for encoder stability
///
pub fn pack_auxiliary_view(yuv444: &Yuv444Frame) -> Yuv420Frame {
    // TESTING: Row-level macroblock packing WITH all-keyframes encoder
    // This will tell us if the packing is correct but P-frame prediction is the issue
    pack_auxiliary_view_spec_compliant(yuv444)

    // Other variants for testing:
    // pack_auxiliary_view_minimal(yuv444)         // All 128s (breaks colors completely)
    // pack_auxiliary_view_simplified(yuv444)      // Pixel-level (also shows lavender)
}

/// Minimal auxiliary view for diagnostic testing
///
/// Returns an auxiliary view with ALL planes set to neutral (128).
/// This effectively disables the auxiliary chroma contribution, making
/// AVC444 behave like AVC420 (reduced color quality but should be corruption-free).
///
/// **Purpose**: Isolate whether corruption is caused by auxiliary view packing.
fn pack_auxiliary_view_minimal(yuv444: &Yuv444Frame) -> Yuv420Frame {
    let width = yuv444.width;
    let height = yuv444.height;

    // All neutral gray (128)
    let y_size = width * height;
    let uv_size = (width / 2) * (height / 2);

    Yuv420Frame {
        y: vec![128u8; y_size],
        u: vec![128u8; uv_size],
        v: vec![128u8; uv_size],
        width,
        height,
    }
}

/// Spec-compliant auxiliary view packing (MS-RDPEGFX Section 3.3.8.3.2)
///
/// Implements the row-level macroblock structure defined in the MS-RDPEGFX specification.
///
/// # Macroblock Structure (16-row alignment)
///
/// The auxiliary Y plane is organized in 16-row macroblocks:
/// - Rows 0-7 of each macroblock: U444 odd rows (1, 3, 5, 7, 9, 11, 13, 15, ...)
/// - Rows 8-15 of each macroblock: V444 odd rows (1, 3, 5, 7, 9, 11, 13, 15, ...)
/// - Pattern repeats every 16 rows for multi-macroblock frames
///
/// Each row is copied ENTIRELY from the source U444/V444 (all columns, not just odd pixels).
/// This row-level structure ensures temporal consistency for H.264 P-frame inter-prediction.
///
/// # Client Reconstruction
///
/// The decoder (FreeRDP, Windows RDP) reads entire rows from auxiliary Y and writes
/// them to U444/V444 odd rows. The row-based structure (not pixel-based) is critical
/// for matching client expectations.
///
/// # Temporal Consistency
///
/// Direct row copying (no interpolation) ensures that static content produces identical
/// auxiliary frames across time, resulting in zero P-frame residuals and no artifacts.
///
/// # References
///
/// - MS-RDPEGFX Section 3.3.8.3.2 (B4/B5 blocks)
/// - FreeRDP: general_YUV444SplitToYUV420 in prim_YUV.c
/// - Microsoft Research: "Tunneling High-Resolution Color Content" (Wu et al., 2013)
fn pack_auxiliary_view_spec_compliant(yuv444: &Yuv444Frame) -> Yuv420Frame {
    let width = yuv444.width;
    let height = yuv444.height;

    // Pad to 16-row macroblock boundary (required by spec)
    // MS-RDPEGFX: "The auxiliary frame is aligned to multiples of 16×16"
    let padded_height = ((height + 15) / 16) * 16;
    // Use explicit allocation + fill to ensure deterministic memory state
    let mut aux_y = vec![0u8; padded_height * width];
    aux_y.fill(128);

    // B4 and B5 blocks: Pack odd rows from U444 and V444
    //
    // MS-RDPEGFX Section 3.3.8.3.2 macroblock structure:
    // - Auxiliary row 0: U444 row 1 (entire row)
    // - Auxiliary row 1: U444 row 3
    // - ...
    // - Auxiliary row 7: U444 row 15
    // - Auxiliary row 8: V444 row 1 (switches to V444)
    // - Auxiliary row 9: V444 row 3
    // - ...
    // - Auxiliary row 15: V444 row 15
    // - Auxiliary row 16: U444 row 17 (pattern repeats)

    let mut u_row_counter = 0; // Tracks which U444 odd row to pack next
    let mut v_row_counter = 0; // Tracks which V444 odd row to pack next

    for aux_row in 0..padded_height {
        let macroblock_row = aux_row % 16;

        if macroblock_row < 8 {
            // Rows 0-7 of macroblock: Pack from U444 odd rows
            let source_row = 2 * u_row_counter + 1;
            u_row_counter += 1;

            // Skip padding rows beyond actual frame
            if source_row >= height {
                continue; // Keep padding as neutral (128)
            }

            // Copy ENTIRE row from U444 (all columns: even and odd!)
            let aux_start = aux_row * width;
            let src_start = source_row * width;
            aux_y[aux_start..aux_start + width]
                .copy_from_slice(&yuv444.u[src_start..src_start + width]);
        } else {
            // Rows 8-15 of macroblock: Pack from V444 odd rows
            let source_row = 2 * v_row_counter + 1;
            v_row_counter += 1;

            // Skip padding rows beyond actual frame
            if source_row >= height {
                continue; // Keep padding as neutral (128)
            }

            // Copy ENTIRE row from V444 (all columns: even and odd!)
            let aux_start = aux_row * width;
            let src_start = source_row * width;
            aux_y[aux_start..aux_start + width]
                .copy_from_slice(&yuv444.v[src_start..src_start + width]);
        }
    }

    // B6 and B7 blocks: Pack chroma from odd columns at even rows
    //
    // MS-RDPEGFX Section 3.3.8.3.2 B6/B7 blocks:
    // - Auxiliary U/V planes are standard YUV420 chroma (halfWidth × halfHeight)
    // - Sample from U444/V444 at positions: (2x+1, 2y) - odd column, even row
    //
    // This captures the chroma values not included in the main view's subsampling.
    let chroma_width = width / 2;
    let chroma_height = height / 2;

    // CRITICAL FIX: Don't pad aux_u/aux_v!
    // Previous code padded to 8x8 boundaries but told encoder unpadded stride.
    // This stride mismatch caused encoder to read wrong memory regions.
    // Let OpenH264 handle any padding it needs internally.

    // Initialize with neutral chroma (128) for deterministic state
    let mut aux_u = vec![0u8; chroma_width * chroma_height];
    let mut aux_v = vec![0u8; chroma_width * chroma_height];
    aux_u.fill(128);
    aux_v.fill(128);

    // Fill actual data with UNPADDED stride (matches what we tell encoder)
    for cy in 0..chroma_height {
        let y = cy * 2; // Even row in source (0, 2, 4, 6, ...)

        for cx in 0..chroma_width {
            let x = cx * 2 + 1; // Odd column in source (1, 3, 5, 7, ...)
            let idx = y * width + x;

            // Write to buffer with UNPADDED stride (matches strides() return value)
            let out_idx = cy * chroma_width + cx; // ← Use chroma_width, not padded!

            // B6: Sample U444 at (odd_col, even_row)
            aux_u[out_idx] = yuv444.u[idx];

            // B7: Sample V444 at (odd_col, even_row)
            aux_v[out_idx] = yuv444.v[idx];

            // DIAGNOSTIC: Log the cycling position
            if width == 1280 && height == 800 && out_idx == 39204 {
                use tracing::debug;
                debug!(
                    "🔍 PACKING aux_u[{}] (cy={}, cx={}) ← yuv444.u[{}] (x={}, y={}): value={}",
                    out_idx, cy, cx, idx, x, y, yuv444.u[idx]
                );
                debug!(
                    "🔍 PACKING aux_v[{}] (cy={}, cx={}) ← yuv444.v[{}] (x={}, y={}): value={}",
                    out_idx, cy, cx, idx, x, y, yuv444.v[idx]
                );
            }
        }
    }

    // DIAGNOSTIC: Multi-position auxiliary view analysis
    use tracing::debug;
    if width == 1280 && height == 800 {
        debug!("═══ AUXILIARY VIEW MULTI-POSITION ANALYSIS ═══");

        // Sample same positions as main view for comparison
        let sample_positions = [
            (0, 0, "Top-Left"),
            (width - 8, 0, "Top-Right"),
            (width / 2, height / 2, "Center"),
        ];

        for (x, y, label) in sample_positions {
            debug!("📍 {} @ ({}, {})", label, x, y);

            // Auxiliary Y: should contain U444 odd rows (0-7) and V444 odd rows (8-15)
            // Check what row of auxiliary Y this position maps to
            let aux_row = y; // Simplified - just check the row directly
            let aux_idx = aux_row * width + x;

            // Sample auxiliary Y plane at this position
            if aux_row < aux_y.len() / width {
                debug!(
                    "  Aux Y (row {}): [{:3},{:3},{:3},{:3}]",
                    aux_row,
                    aux_y.get(aux_idx).unwrap_or(&0),
                    aux_y.get(aux_idx + 1).unwrap_or(&0),
                    aux_y.get(aux_idx + 2).unwrap_or(&0),
                    aux_y.get(aux_idx + 3).unwrap_or(&0)
                );

                // Show corresponding source U444/V444 odd row
                let source_row = if (aux_row % 16) < 8 {
                    // Rows 0-7 of macroblock: from U444 odd rows
                    2 * (aux_row % 16) + 1
                } else {
                    // Rows 8-15: from V444 odd rows
                    2 * ((aux_row % 16) - 8) + 1
                };

                if source_row < height {
                    let src_idx = source_row * width + x;
                    if (aux_row % 16) < 8 {
                        debug!(
                            "  Source U444[row {}]: [{:3},{:3},{:3},{:3}]",
                            source_row,
                            yuv444.u.get(src_idx).unwrap_or(&0),
                            yuv444.u.get(src_idx + 1).unwrap_or(&0),
                            yuv444.u.get(src_idx + 2).unwrap_or(&0),
                            yuv444.u.get(src_idx + 3).unwrap_or(&0)
                        );
                    } else {
                        debug!(
                            "  Source V444[row {}]: [{:3},{:3},{:3},{:3}]",
                            source_row,
                            yuv444.v.get(src_idx).unwrap_or(&0),
                            yuv444.v.get(src_idx + 1).unwrap_or(&0),
                            yuv444.v.get(src_idx + 2).unwrap_or(&0),
                            yuv444.v.get(src_idx + 3).unwrap_or(&0)
                        );
                    }
                }
            }

            // Sample auxiliary U/V chroma at this position
            let chroma_x = x / 2;
            let chroma_y = y / 2;
            let chroma_idx = chroma_y * chroma_width + chroma_x; // Use actual stride
            if chroma_idx < aux_u.len() {
                debug!(
                    "  Aux U420: {:3}, Aux V420: {:3}",
                    aux_u[chroma_idx], aux_v[chroma_idx]
                );

                // Show source position (odd column, even row)
                let src_x = chroma_x * 2 + 1; // Odd column
                let src_y = chroma_y * 2; // Even row
                let src_idx = src_y * width + src_x;
                debug!(
                    "  Source U444[{},{}]: {:3}, V444: {:3}",
                    src_x,
                    src_y,
                    yuv444.u.get(src_idx).unwrap_or(&0),
                    yuv444.v.get(src_idx).unwrap_or(&0)
                );
            }
        }

        debug!("═══════════════════════════════════════════");
    }

    // Trim aux_y to actual frame size (remove 16-row macroblock padding)
    // This ensures aux_y size matches what we tell the encoder (height * width)
    aux_y.truncate(height * width);

    // DIAGNOSTIC: Compute hash AFTER truncate so we only hash what OpenH264 sees
    if width == 1280 && height == 800 {
        use std::hash::{Hash, Hasher};
        use tracing::debug;
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        aux_y.hash(&mut hasher);
        aux_u.hash(&mut hasher);
        aux_v.hash(&mut hasher);
        let frame_hash = hasher.finish();

        use std::sync::atomic::{AtomicU64, Ordering};
        use std::sync::{Mutex, OnceLock};
        static PREV_HASH: AtomicU64 = AtomicU64::new(0);
        static FRAME_NUM: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        static PREV_BUFFERS: OnceLock<Mutex<(Vec<u8>, Vec<u8>, Vec<u8>)>> = OnceLock::new();

        let frame_num = FRAME_NUM.fetch_add(1, Ordering::Relaxed);
        let prev = PREV_HASH.swap(frame_hash, Ordering::Relaxed);

        if prev == frame_hash {
            debug!(
                "[Frame #{}] ✅ TEMPORAL STABLE: Auxiliary IDENTICAL (hash: 0x{:016x})",
                frame_num, frame_hash
            );
        } else if prev != 0 {
            debug!("[Frame #{}] ⚠️  TEMPORAL CHANGE: Auxiliary DIFFERENT (prev: 0x{:016x}, curr: 0x{:016x})", frame_num, prev, frame_hash);

            // OPTION 2: Find first byte that differs
            let buffers =
                PREV_BUFFERS.get_or_init(|| Mutex::new((Vec::new(), Vec::new(), Vec::new())));

            if let Ok(mut prev_bufs) = buffers.lock() {
                let (prev_y, prev_u, prev_v) = &*prev_bufs;

                if !prev_y.is_empty() {
                    // Check aux_y
                    if let Some((idx, old_val, new_val)) = aux_y
                        .iter()
                        .zip(prev_y.iter())
                        .enumerate()
                        .find(|(_, (&a, &b))| a != b)
                        .map(|(i, (&a, &b))| (i, b, a))
                    {
                        let region = if idx < height * width {
                            "DATA"
                        } else {
                            "PADDING"
                        };
                        debug!(
                            "  📍 aux_y[{}] differs: {} (was {}) → {} (now {}) [{}]",
                            idx, old_val, old_val, new_val, new_val, region
                        );
                    }

                    // Check aux_u
                    if let Some((idx, old_val, new_val)) = aux_u
                        .iter()
                        .zip(prev_u.iter())
                        .enumerate()
                        .find(|(_, (&a, &b))| a != b)
                        .map(|(i, (&a, &b))| (i, b, a))
                    {
                        let data_size = chroma_height * chroma_width;
                        let region = if idx < data_size { "DATA" } else { "PADDING" };
                        let row = idx / chroma_width; // Use actual stride, not padded
                        let col = idx % chroma_width;
                        debug!(
                            "  📍 aux_u[{}] (row {}, col {}) differs: {} → {} [{}]",
                            idx, row, col, old_val, new_val, region
                        );
                        debug!(
                            "     (chroma_width={}, data_size={})",
                            chroma_width, data_size
                        );
                    }

                    // Check aux_v
                    if let Some((idx, old_val, new_val)) = aux_v
                        .iter()
                        .zip(prev_v.iter())
                        .enumerate()
                        .find(|(_, (&a, &b))| a != b)
                        .map(|(i, (&a, &b))| (i, b, a))
                    {
                        let data_size = chroma_height * chroma_width;
                        let region = if idx < data_size { "DATA" } else { "PADDING" };
                        let row = idx / chroma_width; // Use actual stride, not padded
                        let col = idx % chroma_width;
                        debug!(
                            "  📍 aux_v[{}] (row {}, col {}) differs: {} → {} [{}]",
                            idx, row, col, old_val, new_val, region
                        );
                        debug!(
                            "     (chroma_width={}, data_size={})",
                            chroma_width, data_size
                        );
                    }
                }

                // Store current buffers for next comparison
                *prev_bufs = (aux_y.clone(), aux_u.clone(), aux_v.clone());
            }
        } else {
            debug!(
                "[Frame #{}] 🔵 FIRST FRAME: Auxiliary hash: 0x{:016x}",
                frame_num, frame_hash
            );

            // Store first frame buffers
            let buffers =
                PREV_BUFFERS.get_or_init(|| Mutex::new((Vec::new(), Vec::new(), Vec::new())));

            if let Ok(mut prev_bufs) = buffers.lock() {
                *prev_bufs = (aux_y.clone(), aux_u.clone(), aux_v.clone());
            }
        }
    }

    Yuv420Frame {
        y: aux_y, // Luma buffer (height * width)
        u: aux_u, // Chroma buffer (chroma_width * chroma_height = width/2 * height/2)
        v: aux_v, // Chroma buffer (chroma_width * chroma_height = width/2 * height/2)
        width,
        height,
    }
}

/// Historical: Old pixel-level packing approach (REMOVED - caused P-frame corruption)
///
/// This was the original implementation that used pixel-level odd/even filtering
/// with interpolation at even positions. It caused lavender corruption in P-frames
/// due to frame-dependent interpolated values creating artificial temporal changes.
///
/// Replaced with row-level macroblock packing (2025-12-27) to match MS-RDPEGFX spec
/// and eliminate P-frame artifacts. See docs/AVC444-COMPREHENSIVE-RESEARCH-AND-FIX-2025-12-27.md
///
/// Alternative simplified packing for debugging/testing
///
/// Uses a simpler pixel extraction pattern that may not match the spec
/// exactly but is easier to debug.
#[allow(dead_code)]
pub fn pack_auxiliary_view_simplified(yuv444: &Yuv444Frame) -> Yuv420Frame {
    let width = yuv444.width;
    let height = yuv444.height;

    // Y plane: All U444 values at odd positions, 128 at even
    let mut aux_y = Vec::with_capacity(width * height);

    for y in 0..height {
        for x in 0..width {
            let idx = y * width + x;
            if x % 2 == 1 || y % 2 == 1 {
                aux_y.push(yuv444.u[idx]);
            } else {
                aux_y.push(128); // Neutral
            }
        }
    }

    // U plane: Just take one V sample per 2×2 block
    let chroma_width = width / 2;
    let chroma_height = height / 2;
    let mut aux_u = Vec::with_capacity(chroma_width * chroma_height);

    for y in (0..height).step_by(2) {
        for x in (0..width).step_by(2) {
            // Take the (x+1, y+1) sample (diagonal)
            let idx = (y + 1) * width + (x + 1);
            aux_u.push(yuv444.v[idx]);
        }
    }

    // V plane: Neutral
    let aux_v = vec![128u8; chroma_width * chroma_height];

    Yuv420Frame {
        y: aux_y,
        u: aux_u,
        v: aux_v,
        width,
        height,
    }
}

pub fn pack_dual_views(yuv444: &Yuv444Frame) -> (Yuv420Frame, Yuv420Frame) {
    let main_view = pack_main_view(yuv444);
    let aux_view = pack_auxiliary_view(yuv444);
    (main_view, aux_view)
}

/// Validate that a YUV444 frame has valid dimensions for AVC444 encoding
///
/// Returns true if dimensions are even (required for 4:2:0 subsampling).
#[inline]
pub fn validate_dimensions(width: usize, height: usize) -> bool {
    width % 2 == 0 && height % 2 == 0 && width > 0 && height > 0
}

/// Align dimension to 16-pixel boundary (macroblock alignment)
///
/// H.264 encoding works with 16×16 macroblocks, so dimensions should
/// be aligned for optimal encoding.
#[inline]
pub fn align_to_16(dim: usize) -> usize {
    (dim + 15) & !15
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_yuv444(width: usize, height: usize) -> Yuv444Frame {
        let size = width * height;
        let mut y = vec![128u8; size];
        let mut u = vec![128u8; size];
        let mut v = vec![128u8; size];

        // Create a gradient pattern for testing
        for row in 0..height {
            for col in 0..width {
                let idx = row * width + col;
                y[idx] = ((col * 255) / width) as u8;
                u[idx] = ((row * 255) / height) as u8;
                v[idx] = (((col + row) * 255) / (width + height)) as u8;
            }
        }

        Yuv444Frame {
            y,
            u,
            v,
            width,
            height,
        }
    }

    #[test]
    fn test_pack_main_view_dimensions() {
        let yuv444 = create_test_yuv444(1920, 1080);
        let main = pack_main_view(&yuv444);

        // Y plane: full resolution
        assert_eq!(main.y.len(), 1920 * 1080);
        assert_eq!(main.width, 1920);
        assert_eq!(main.height, 1080);

        // U/V planes: half resolution in each dimension (no padding for main view)
        // openh264 handles macroblock alignment internally
        assert_eq!(main.u.len(), 960 * 540);
        assert_eq!(main.v.len(), 960 * 540);
    }

    #[test]
    fn test_pack_main_view_luma_unchanged() {
        let yuv444 = create_test_yuv444(64, 64);
        let main = pack_main_view(&yuv444);

        // Luma should be identical to source
        assert_eq!(main.y, yuv444.y);
    }

    #[test]
    fn test_pack_auxiliary_view_dimensions() {
        let yuv444 = create_test_yuv444(1920, 1080);
        let aux = pack_auxiliary_view(&yuv444);

        // Y plane: full resolution (contains U444 data)
        assert_eq!(aux.y.len(), 1920 * 1080);
        assert_eq!(aux.width, 1920);
        assert_eq!(aux.height, 1080);

        // U/V planes: half resolution
        assert_eq!(aux.u.len(), 960 * 540);
        assert_eq!(aux.v.len(), 960 * 540);
    }

    #[test]
    fn test_auxiliary_view_row_macroblock_structure() {
        // Spec-compliant auxiliary view uses row-level macroblock structure,
        // not pixel-level packing. This test verifies the row structure.
        let yuv444 = create_test_yuv444(64, 64);
        let aux = pack_auxiliary_view(&yuv444);

        // Auxiliary Y contains:
        // - Rows 0-7 of macroblock: U444 odd rows (1, 3, 5, 7, ...)
        // - Rows 8-15 of macroblock: V444 odd rows (1, 3, 5, 7, ...)

        // Verify first macroblock structure (64x64 has 4 macroblocks vertically)
        // Row 0 of aux should be row 1 of U444
        for x in 0..64 {
            let aux_idx = 0 * 64 + x; // Row 0
            let u444_idx = 1 * 64 + x; // Row 1 of U444 (odd row)
            assert_eq!(
                aux.y[aux_idx], yuv444.u[u444_idx],
                "Auxiliary Y row 0 should be U444 row 1"
            );
        }

        // Row 8 of aux should be row 1 of V444
        for x in 0..64 {
            let aux_idx = 8 * 64 + x; // Row 8
            let v444_idx = 1 * 64 + x; // Row 1 of V444 (odd row)
            assert_eq!(
                aux.y[aux_idx], yuv444.v[v444_idx],
                "Auxiliary Y row 8 should be V444 row 1"
            );
        }
    }

    #[test]
    fn test_pack_dual_views() {
        let yuv444 = create_test_yuv444(320, 240);
        let (main, aux) = pack_dual_views(&yuv444);

        assert_eq!(main.width, 320);
        assert_eq!(main.height, 240);
        assert_eq!(aux.width, 320);
        assert_eq!(aux.height, 240);
    }

    #[test]
    fn test_yuv420_to_bgra_black() {
        let yuv420 = Yuv420Frame {
            y: vec![0; 4],   // 2×2 black
            u: vec![128; 1], // 1×1 neutral chroma
            v: vec![128; 1],
            width: 2,
            height: 2,
        };

        let bgra = yuv420.to_bgra();

        assert_eq!(bgra.len(), 4 * 4); // 4 pixels × 4 bytes

        // All pixels should be black (B=0, G=0, R=0, A=255)
        for i in 0..4 {
            assert_eq!(bgra[i * 4], 0, "Pixel {} B should be 0", i);
            assert_eq!(bgra[i * 4 + 1], 0, "Pixel {} G should be 0", i);
            assert_eq!(bgra[i * 4 + 2], 0, "Pixel {} R should be 0", i);
            assert_eq!(bgra[i * 4 + 3], 255, "Pixel {} A should be 255", i);
        }
    }

    #[test]
    fn test_yuv420_to_bgra_white() {
        let yuv420 = Yuv420Frame {
            y: vec![255; 4], // 2×2 white
            u: vec![128; 1], // 1×1 neutral chroma
            v: vec![128; 1],
            width: 2,
            height: 2,
        };

        let bgra = yuv420.to_bgra();

        // All pixels should be white (B=255, G=255, R=255, A=255)
        for i in 0..4 {
            assert_eq!(bgra[i * 4], 255, "Pixel {} B should be 255", i);
            assert_eq!(bgra[i * 4 + 1], 255, "Pixel {} G should be 255", i);
            assert_eq!(bgra[i * 4 + 2], 255, "Pixel {} R should be 255", i);
            assert_eq!(bgra[i * 4 + 3], 255, "Pixel {} A should be 255", i);
        }
    }

    #[test]
    fn test_validate_dimensions() {
        assert!(validate_dimensions(1920, 1080));
        assert!(validate_dimensions(1280, 720));
        assert!(validate_dimensions(2, 2));

        assert!(!validate_dimensions(1921, 1080)); // Odd width
        assert!(!validate_dimensions(1920, 1081)); // Odd height
        assert!(!validate_dimensions(0, 100)); // Zero width
        assert!(!validate_dimensions(100, 0)); // Zero height
    }

    #[test]
    fn test_align_to_16() {
        assert_eq!(align_to_16(1920), 1920); // Already aligned
        assert_eq!(align_to_16(1080), 1088); // Needs padding
        assert_eq!(align_to_16(800), 800); // Already aligned
        assert_eq!(align_to_16(600), 608); // Needs padding
        assert_eq!(align_to_16(1), 16);
        assert_eq!(align_to_16(15), 16);
        assert_eq!(align_to_16(16), 16);
        assert_eq!(align_to_16(17), 32);
    }

    #[test]
    fn test_yuv420_frame_new() {
        let frame = Yuv420Frame::new(1920, 1080);

        assert_eq!(frame.width, 1920);
        assert_eq!(frame.height, 1080);
        assert_eq!(frame.y.len(), 1920 * 1080);
        assert_eq!(frame.u.len(), 960 * 540);
        assert_eq!(frame.v.len(), 960 * 540);

        // Y should be 0, U/V should be 128 (neutral)
        assert!(frame.y.iter().all(|&v| v == 0));
        assert!(frame.u.iter().all(|&v| v == 128));
        assert!(frame.v.iter().all(|&v| v == 128));
    }

    #[test]
    fn test_yuv420_total_size() {
        let frame = Yuv420Frame::new(1920, 1080);
        // Y: 1920×1080, U: 960×540, V: 960×540
        // Total: 1920*1080 + 2*(960*540) = 2073600 + 1036800 = 3110400
        assert_eq!(frame.total_size(), 1920 * 1080 + 2 * (960 * 540));
    }

    // NOTE: test_interpolate_even_position_center removed - function was removed
    // during aux omission refactoring (interpolation no longer used)

    #[test]
    fn test_pack_and_unpack_roundtrip() {
        // Create a test pattern
        let yuv444 = create_test_yuv444(64, 64);

        // Pack to dual views
        let (main, aux) = pack_dual_views(&yuv444);

        // Convert both to BGRA
        let main_bgra = main.to_bgra();
        let aux_bgra = aux.to_bgra();

        // Verify dimensions
        assert_eq!(main_bgra.len(), 64 * 64 * 4);
        assert_eq!(aux_bgra.len(), 64 * 64 * 4);
    }

    // =================================================================
    // YUV420Frame Plane Accessor Tests (for OpenH264 compatibility)
    // =================================================================

    #[test]
    fn test_yuv420_plane_accessors() {
        let frame = Yuv420Frame::new(1920, 1080);

        // Test dimensions
        let (w, h) = frame.dimensions();
        assert_eq!(w, 1920);
        assert_eq!(h, 1080);

        // Test strides
        let (y_stride, u_stride, v_stride) = frame.strides();
        assert_eq!(y_stride, 1920);
        assert_eq!(u_stride, 960);
        assert_eq!(v_stride, 960);

        // Test plane lengths
        assert_eq!(frame.y_plane().len(), 1920 * 1080);
        assert_eq!(frame.u_plane().len(), 960 * 540);
        assert_eq!(frame.v_plane().len(), 960 * 540);
    }

    #[test]
    fn test_yuv420_validate_for_encoding_valid() {
        let frame = Yuv420Frame::new(1920, 1080);
        assert!(frame.validate_for_encoding().is_ok());

        let frame2 = Yuv420Frame::new(1280, 720);
        assert!(frame2.validate_for_encoding().is_ok());

        let frame3 = Yuv420Frame::new(640, 480);
        assert!(frame3.validate_for_encoding().is_ok());
    }

    #[test]
    fn test_yuv420_validate_for_encoding_invalid_width() {
        // Create a frame with invalid (odd) width by modifying internals
        let mut frame = Yuv420Frame::new(1920, 1080);
        frame.width = 1921; // Invalid odd width

        let result = frame.validate_for_encoding();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Width"));
    }

    #[test]
    fn test_yuv420_validate_for_encoding_invalid_height() {
        let mut frame = Yuv420Frame::new(1920, 1080);
        frame.height = 1081; // Invalid odd height

        let result = frame.validate_for_encoding();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Height"));
    }

    // =================================================================
    // Pack Main View Tests
    // =================================================================

    #[test]
    fn test_pack_main_view_1080p() {
        let yuv444 = create_test_yuv444(1920, 1080);
        let main = pack_main_view(&yuv444);

        assert_eq!(main.width, 1920);
        assert_eq!(main.height, 1080);
        assert_eq!(main.y.len(), 1920 * 1080);

        // Chroma planes: half resolution (no padding - openh264 handles alignment)
        assert_eq!(main.u.len(), 960 * 540);
        assert_eq!(main.v.len(), 960 * 540);

        // Y plane should be identical
        assert_eq!(main.y, yuv444.y);
    }

    #[test]
    fn test_pack_main_view_luma_preserved() {
        // Create frame with distinct luma pattern
        let mut yuv444 = Yuv444Frame::new(16, 16);
        for i in 0..256 {
            yuv444.y[i] = i as u8;
        }

        let main = pack_main_view(&yuv444);

        // Luma should be perfectly preserved
        for i in 0..256 {
            assert_eq!(main.y[i], i as u8, "Luma mismatch at {}", i);
        }
    }

    // =================================================================
    // Pack Auxiliary View Tests
    // =================================================================

    #[test]
    fn test_pack_auxiliary_view_1080p() {
        let yuv444 = create_test_yuv444(1920, 1080);
        let aux = pack_auxiliary_view(&yuv444);

        assert_eq!(aux.width, 1920);
        assert_eq!(aux.height, 1080);
        assert_eq!(aux.y.len(), 1920 * 1080);
        assert_eq!(aux.u.len(), 960 * 540);
        assert_eq!(aux.v.len(), 960 * 540);
    }

    #[test]
    fn test_pack_auxiliary_view_has_chroma_data() {
        let yuv444 = create_test_yuv444(64, 64);
        let aux = pack_auxiliary_view(&yuv444);

        // Spec-compliant auxiliary view includes sampled V444 data in aux.v
        // (not neutral 128 - that's only the "minimal" variant)
        // The V plane contains samples from (odd_col, even_row) positions of V444

        // Check that aux_v has non-neutral values (not all 128)
        let has_non_neutral = aux.v.iter().any(|&v| v != 128);
        assert!(
            has_non_neutral,
            "Auxiliary V plane should contain sampled V444 data, not all neutral"
        );

        // Verify aux_v contains values from yuv444.v gradient
        // create_test_yuv444 creates V gradient, so aux should have some of those values
        let min = aux.v.iter().min().copied().unwrap_or(128);
        let max = aux.v.iter().max().copied().unwrap_or(128);
        assert!(
            max > min,
            "Auxiliary V plane should have variation from V444 gradient"
        );
    }

    // =================================================================
    // Dual View Consistency Tests
    // =================================================================

    #[test]
    fn test_dual_views_same_dimensions() {
        let yuv444 = create_test_yuv444(320, 240);
        let (main, aux) = pack_dual_views(&yuv444);

        assert_eq!(main.width, aux.width);
        assert_eq!(main.height, aux.height);
        assert_eq!(main.y.len(), aux.y.len());
        assert_eq!(main.u.len(), aux.u.len());
        assert_eq!(main.v.len(), aux.v.len());
    }

    #[test]
    fn test_dual_views_uniform_input() {
        // Uniform color should produce consistent output
        let mut yuv444 = Yuv444Frame::new(8, 8);
        for i in 0..64 {
            yuv444.y[i] = 128;
            yuv444.u[i] = 100;
            yuv444.v[i] = 150;
        }

        let (main, aux) = pack_dual_views(&yuv444);

        // Main Y should be 128
        assert!(main.y.iter().all(|&v| v == 128));

        // Main U/V should be uniform (no padding in main view)
        // For 8x8 input: chroma is 4x4=16 elements
        assert!(
            main.u.iter().all(|&v| v == 100),
            "Chroma U should be 100 (uniform input)"
        );

        assert!(
            main.v.iter().all(|&v| v == 150),
            "Chroma V should be 150 (uniform input)"
        );
    }

    // =================================================================
    // to_bgra Roundtrip Tests
    // =================================================================

    #[test]
    fn test_yuv420_to_bgra_dimensions() {
        let frame = Yuv420Frame::new(640, 480);
        let bgra = frame.to_bgra();

        // Should have 4 bytes per pixel
        assert_eq!(bgra.len(), 640 * 480 * 4);
    }

    #[test]
    fn test_yuv420_to_bgra_neutral_color() {
        // Y=128, U=128, V=128 should produce gray
        let mut frame = Yuv420Frame::new(2, 2);
        frame.y = vec![128; 4];
        frame.u = vec![128; 1];
        frame.v = vec![128; 1];

        let bgra = frame.to_bgra();

        // All pixels should be approximately gray (128, 128, 128)
        for i in 0..4 {
            let b = bgra[i * 4];
            let g = bgra[i * 4 + 1];
            let r = bgra[i * 4 + 2];
            let a = bgra[i * 4 + 3];

            assert!((b as i32 - 128).abs() <= 2, "B should be ~128");
            assert!((g as i32 - 128).abs() <= 2, "G should be ~128");
            assert!((r as i32 - 128).abs() <= 2, "R should be ~128");
            assert_eq!(a, 255, "A should be 255");
        }
    }

    // =================================================================
    // Stress Tests
    // =================================================================

    #[test]
    fn test_pack_dual_views_stress() {
        // Test various resolutions
        let resolutions = [(64, 64), (320, 240), (640, 480), (1280, 720)];

        for (w, h) in resolutions {
            let yuv444 = create_test_yuv444(w, h);
            let (main, aux) = pack_dual_views(&yuv444);

            assert_eq!(main.width, w);
            assert_eq!(main.height, h);
            assert_eq!(aux.width, w);
            assert_eq!(aux.height, h);

            // Both should be valid for encoding
            assert!(main.validate_for_encoding().is_ok());
            assert!(aux.validate_for_encoding().is_ok());
        }
    }
}
