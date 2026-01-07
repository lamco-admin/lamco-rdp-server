//! Color space conversion for AVC444
//!
//! Converts BGRA to YUV444 using ITU-R BT.709 (HD) or BT.601 (SD) matrices.
//!
//! # Performance
//!
//! This module includes SIMD-optimized implementations:
//! - **AVX2** for x86_64: Processes 8 pixels per iteration (~4× speedup)
//! - **NEON** for AArch64: Processes 8 pixels per iteration (~4× speedup)
//! - Scalar fallback for all platforms
//!
//! # Color Space Standards
//!
//! - **BT.709**: ITU-R Rec. 709 for HD content (1080p and above)
//! - **BT.601**: ITU-R Rec. 601 for SD content (720p and below)
//!
//! # MS-RDPEGFX Reference
//!
//! Color conversion follows the formulas in MS-RDPEGFX Section 3.3.8.3.

/// Color matrix standard for RGB to YUV conversion
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorMatrix {
    /// ITU-R BT.601 (SD content, <=720p) - FULL RANGE
    /// Y =  0.299  R + 0.587  G + 0.114  B
    BT601,
    /// ITU-R BT.709 (HD content, >=1080p) - FULL RANGE
    /// Y =  0.2126 R + 0.7152 G + 0.0722 B
    #[default]
    BT709,
    /// OpenH264-compatible conversion - LIMITED RANGE (BT.601)
    /// Matches OpenH264's internal RGB→YUV conversion exactly.
    /// CRITICAL: Use this for AVC444 when feeding YUVSlices directly!
    ///
    /// Y = (66*R + 129*G + 25*B) / 256 + 16  (range: 16-235)
    /// U = (-38*R - 74*G + 112*B) / 256 + 128  (range: 16-240)
    /// V = (112*R - 94*G - 18*B) / 256 + 128  (range: 16-240)
    OpenH264,
}

impl ColorMatrix {
    /// Auto-select color matrix based on resolution
    ///
    /// Uses BT.709 for HD content (width >= 1280 AND height >= 720)
    /// Uses BT.601 for SD content (smaller resolutions)
    ///
    /// The threshold is 720p (1280×720), which is the standard HD boundary.
    /// Resolutions like 1024×768 (XGA) are considered SD despite having
    /// height >= 720, because the aspect ratio indicates legacy SD content.
    #[inline]
    pub fn auto_select(width: u32, height: u32) -> Self {
        if width >= 1280 && height >= 720 {
            Self::BT709
        } else {
            Self::BT601
        }
    }

    /// Get RGB to Y (luma) coefficients as fixed-point (16.16 format)
    /// Returns (Kr, Kg, Kb) scaled by 65536
    #[inline]
    const fn y_coefficients_fixed(&self) -> (i32, i32, i32) {
        match self {
            Self::BT601 => (19595, 38470, 7471), // 0.299, 0.587, 0.114
            Self::BT709 => (13933, 46871, 4732), // 0.2126, 0.7152, 0.0722
            // OpenH264: 66/256, 129/256, 25/256 → scaled by 65536/256 = 256
            Self::OpenH264 => (16896, 33024, 6400), // 66*256, 129*256, 25*256
        }
    }

    /// Returns true if this matrix uses limited range (Y: 16-235)
    #[inline]
    pub const fn is_limited_range(&self) -> bool {
        matches!(self, Self::OpenH264)
    }

    /// Get RGB to U (Cb) coefficients as fixed-point (16.16 format)
    /// Formula: U = -0.5*Kr/(1-Kb)*R - 0.5*Kg/(1-Kb)*G + 0.5*B + 128
    #[inline]
    const fn u_coefficients_fixed(&self) -> (i32, i32, i32) {
        match self {
            // BT.601: U = -0.1687 R - 0.3313 G + 0.5 B + 128
            Self::BT601 => (-11056, -21712, 32768),
            // BT.709: U = -0.1146 R - 0.3854 G + 0.5 B + 128
            Self::BT709 => (-7508, -25260, 32768),
            // OpenH264: -38/256, -74/256, 112/256 → scaled by 256
            Self::OpenH264 => (-9728, -18944, 28672), // -38*256, -74*256, 112*256
        }
    }

    /// Get RGB to V (Cr) coefficients as fixed-point (16.16 format)
    /// Formula: V = 0.5*R - 0.5*Kg/(1-Kr)*G - 0.5*Kb/(1-Kr)*B + 128
    #[inline]
    const fn v_coefficients_fixed(&self) -> (i32, i32, i32) {
        match self {
            // BT.601: V = 0.5 R - 0.4187 G - 0.0813 B + 128
            Self::BT601 => (32768, -27440, -5328),
            // BT.709: V = 0.5 R - 0.4542 G - 0.0458 B + 128
            Self::BT709 => (32768, -29764, -3004),
            // OpenH264: 112/256, -94/256, -18/256 → scaled by 256
            Self::OpenH264 => (28672, -24064, -4608), // 112*256, -94*256, -18*256
        }
    }
}

/// YUV444 frame (full chroma resolution)
///
/// Each plane has the same dimensions (width × height).
/// This preserves full color information before packing into dual YUV420 streams.
#[derive(Debug, Clone)]
pub struct Yuv444Frame {
    /// Luma plane (width × height)
    pub y: Vec<u8>,
    /// Chroma U (Cb) plane (width × height)
    pub u: Vec<u8>,
    /// Chroma V (Cr) plane (width × height)
    pub v: Vec<u8>,
    /// Frame width in pixels
    pub width: usize,
    /// Frame height in pixels
    pub height: usize,
}

impl Yuv444Frame {
    /// Create a new YUV444 frame with allocated buffers
    pub fn new(width: usize, height: usize) -> Self {
        let size = width * height;
        Self {
            y: vec![0u8; size],
            u: vec![128u8; size], // Neutral chroma
            v: vec![128u8; size], // Neutral chroma
            width,
            height,
        }
    }

    /// Get the pixel count
    #[inline]
    pub fn pixel_count(&self) -> usize {
        self.width * self.height
    }
}

/// Convert BGRA to YUV444
///
/// # Arguments
///
/// * `bgra` - BGRA pixel data (4 bytes per pixel, row-major: B, G, R, A)
/// * `width` - Frame width in pixels
/// * `height` - Frame height in pixels
/// * `matrix` - Color matrix standard (BT.601 or BT.709)
///
/// # Returns
///
/// YUV444 frame with full chroma resolution
///
/// # Panics
///
/// Panics if `bgra.len() != width * height * 4`
pub fn bgra_to_yuv444(
    bgra: &[u8],
    width: usize,
    height: usize,
    matrix: ColorMatrix,
) -> Yuv444Frame {
    let pixel_count = width * height;
    assert_eq!(
        bgra.len(),
        pixel_count * 4,
        "BGRA buffer size mismatch: expected {}, got {}",
        pixel_count * 4,
        bgra.len()
    );

    let mut frame = Yuv444Frame::new(width, height);

    // SIMD is enabled for full-range color matrices (BT.601/BT.709).
    // For limited range (OpenH264), we use the scalar implementation
    // which correctly handles Y: 16-235, UV: 16-240 clamping.
    let use_simd = true;

    if use_simd && !matrix.is_limited_range() {
        // Dispatch to best available SIMD implementation for full range
        #[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
        {
            bgra_to_yuv444_avx2(bgra, &mut frame, matrix);
            return frame;
        }

        #[cfg(all(target_arch = "aarch64", target_feature = "neon"))]
        {
            bgra_to_yuv444_neon(bgra, &mut frame, matrix);
            return frame;
        }

        // Runtime feature detection for x86_64
        #[cfg(target_arch = "x86_64")]
        {
            if is_x86_feature_detected!("avx2") {
                // SAFETY: We verified AVX2 is available
                unsafe { bgra_to_yuv444_avx2_impl(bgra, &mut frame, matrix) };
                return frame;
            }
        }
    }

    // Scalar fallback - handles both full and limited range correctly
    bgra_to_yuv444_scalar(bgra, &mut frame, matrix);

    frame
}

/// Scalar implementation of BGRA to YUV444 conversion
///
/// Uses fixed-point arithmetic for performance while maintaining precision.
/// Supports both full range (BT.601/BT.709) and limited range (OpenH264).
fn bgra_to_yuv444_scalar(bgra: &[u8], frame: &mut Yuv444Frame, matrix: ColorMatrix) {
    let (y_kr, y_kg, y_kb) = matrix.y_coefficients_fixed();
    let (u_kr, u_kg, u_kb) = matrix.u_coefficients_fixed();
    let (v_kr, v_kg, v_kb) = matrix.v_coefficients_fixed();
    let limited_range = matrix.is_limited_range();

    let pixel_count = frame.pixel_count();

    for i in 0..pixel_count {
        let b = bgra[i * 4] as i32;
        let g = bgra[i * 4 + 1] as i32;
        let r = bgra[i * 4 + 2] as i32;
        // Alpha (bgra[i * 4 + 3]) is ignored

        // Y = Kr*R + Kg*G + Kb*B
        // Fixed-point: multiply then shift right 16 bits, add 0.5 for rounding
        // For limited range (OpenH264): add +16 offset (Y range: 16-235)
        let y_base = (y_kr * r + y_kg * g + y_kb * b + 32768) >> 16;
        let y_val = if limited_range {
            (y_base + 16).clamp(16, 235)
        } else {
            y_base.clamp(0, 255)
        };

        // U = Ur*R + Ug*G + Ub*B + 128
        let u_val = ((u_kr * r + u_kg * g + u_kb * b + 32768) >> 16) + 128;
        let u_val = if limited_range {
            u_val.clamp(16, 240)
        } else {
            u_val.clamp(0, 255)
        };

        // V = Vr*R + Vg*G + Vb*B + 128
        let v_val = ((v_kr * r + v_kg * g + v_kb * b + 32768) >> 16) + 128;
        let v_val = if limited_range {
            v_val.clamp(16, 240)
        } else {
            v_val.clamp(0, 255)
        };

        frame.y[i] = y_val as u8;
        frame.u[i] = u_val as u8;
        frame.v[i] = v_val as u8;
    }
}

/// AVX2 implementation of BGRA to YUV444 conversion (x86_64)
///
/// Processes 8 pixels per iteration using 256-bit SIMD registers.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn bgra_to_yuv444_avx2_impl(bgra: &[u8], frame: &mut Yuv444Frame, matrix: ColorMatrix) {
    unsafe {
        use std::arch::x86_64::{
            __m128i, __m256i, _mm256_add_epi32, _mm256_castsi256_si128, _mm256_extracti128_si256,
            _mm256_loadu_si256, _mm256_max_epi32, _mm256_min_epi32, _mm256_mullo_epi32,
            _mm256_packs_epi32, _mm256_packus_epi16, _mm256_set1_epi32, _mm256_set_m128i,
            _mm256_setzero_si256, _mm256_srai_epi32, _mm_setr_epi8, _mm_setzero_si128,
            _mm_shuffle_epi8, _mm_storel_epi64, _mm_unpackhi_epi16, _mm_unpacklo_epi16,
            _mm_unpacklo_epi32, _mm_unpacklo_epi8,
        };

        let (y_kr, y_kg, y_kb) = matrix.y_coefficients_fixed();
        let (u_kr, u_kg, u_kb) = matrix.u_coefficients_fixed();
        let (v_kr, v_kg, v_kb) = matrix.v_coefficients_fixed();

        let pixel_count = frame.pixel_count();
        let simd_count = pixel_count / 8; // Process 8 pixels at a time
        let remainder = pixel_count % 8;

        // Broadcast coefficients to all lanes
        let y_kr_v = _mm256_set1_epi32(y_kr);
        let y_kg_v = _mm256_set1_epi32(y_kg);
        let y_kb_v = _mm256_set1_epi32(y_kb);

        let u_kr_v = _mm256_set1_epi32(u_kr);
        let u_kg_v = _mm256_set1_epi32(u_kg);
        let u_kb_v = _mm256_set1_epi32(u_kb);

        let v_kr_v = _mm256_set1_epi32(v_kr);
        let v_kg_v = _mm256_set1_epi32(v_kg);
        let v_kb_v = _mm256_set1_epi32(v_kb);

        let half = _mm256_set1_epi32(32768); // 0.5 in fixed-point for rounding
        let offset_128 = _mm256_set1_epi32(128);
        let zero = _mm256_setzero_si256();
        let max_255 = _mm256_set1_epi32(255);

        for i in 0..simd_count {
            let base = i * 8;
            let bgra_offset = base * 4;

            // Load 8 BGRA pixels (32 bytes)
            // BGRA layout: [B0,G0,R0,A0, B1,G1,R1,A1, ...]
            let bgra_ptr = bgra.as_ptr().add(bgra_offset);
            let pixels = _mm256_loadu_si256(bgra_ptr as *const __m256i);

            // Extract B, G, R channels (skip A)
            // We need to deinterleave BGRA to separate B, G, R

            // Shuffle mask to extract bytes: indices 0,4,8,12,16,20,24,28 for each channel
            // This is complex in AVX2, use a simpler approach with unpack

            // Extract low and high 128-bit lanes
            let lo = _mm256_castsi256_si128(pixels);
            let hi = _mm256_extracti128_si256(pixels, 1);

            // Process pixels 0-3 (low lane) and 4-7 (high lane) separately
            // Each pixel is 4 bytes: BGRA

            // For simplicity and correctness, process 4 pixels at a time in each lane
            // Pixel 0: bytes 0-3, Pixel 1: bytes 4-7, Pixel 2: bytes 8-11, Pixel 3: bytes 12-15

            // Extract each channel using shuffle
            // B: indices 0, 4, 8, 12 -> shuffle bytes
            let shuffle_b =
                _mm_setr_epi8(0, 4, 8, 12, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1);
            let shuffle_g =
                _mm_setr_epi8(1, 5, 9, 13, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1);
            let shuffle_r =
                _mm_setr_epi8(2, 6, 10, 14, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1);

            // Extract from low lane (pixels 0-3)
            let b_lo = _mm_shuffle_epi8(lo, shuffle_b);
            let g_lo = _mm_shuffle_epi8(lo, shuffle_g);
            let r_lo = _mm_shuffle_epi8(lo, shuffle_r);

            // Extract from high lane (pixels 4-7)
            let b_hi = _mm_shuffle_epi8(hi, shuffle_b);
            let g_hi = _mm_shuffle_epi8(hi, shuffle_g);
            let r_hi = _mm_shuffle_epi8(hi, shuffle_r);

            // Combine low and high into 8-element vectors
            // Pack B0-B3 in low dword, B4-B7 in next dword
            let b_packed = _mm_unpacklo_epi32(b_lo, b_hi); // B0B1B2B3 B4B5B6B7 in low 8 bytes
            let g_packed = _mm_unpacklo_epi32(g_lo, g_hi);
            let r_packed = _mm_unpacklo_epi32(r_lo, r_hi);

            // Zero-extend bytes to 32-bit integers for multiplication
            // Use unpack with zero to get 32-bit values
            let zero_128 = _mm_setzero_si128();

            // Unpack to 16-bit first, then to 32-bit
            let b_16 = _mm_unpacklo_epi8(b_packed, zero_128); // 8x 16-bit
            let g_16 = _mm_unpacklo_epi8(g_packed, zero_128);
            let r_16 = _mm_unpacklo_epi8(r_packed, zero_128);

            // Extend to 32-bit (low 4 and high 4)
            let b_32_lo = _mm_unpacklo_epi16(b_16, zero_128);
            let b_32_hi = _mm_unpackhi_epi16(b_16, zero_128);
            let g_32_lo = _mm_unpacklo_epi16(g_16, zero_128);
            let g_32_hi = _mm_unpackhi_epi16(g_16, zero_128);
            let r_32_lo = _mm_unpacklo_epi16(r_16, zero_128);
            let r_32_hi = _mm_unpackhi_epi16(r_16, zero_128);

            // Combine into 256-bit vectors
            let b_32 = _mm256_set_m128i(b_32_hi, b_32_lo);
            let g_32 = _mm256_set_m128i(g_32_hi, g_32_lo);
            let r_32 = _mm256_set_m128i(r_32_hi, r_32_lo);

            // Calculate Y = Kr*R + Kg*G + Kb*B (fixed-point)
            let y_r = _mm256_mullo_epi32(r_32, y_kr_v);
            let y_g = _mm256_mullo_epi32(g_32, y_kg_v);
            let y_b = _mm256_mullo_epi32(b_32, y_kb_v);
            let y_sum = _mm256_add_epi32(_mm256_add_epi32(y_r, y_g), y_b);
            let y_sum = _mm256_add_epi32(y_sum, half); // Add 0.5 for rounding
            let y_val = _mm256_srai_epi32(y_sum, 16); // Shift right 16
            let y_val = _mm256_max_epi32(y_val, zero);
            let y_val = _mm256_min_epi32(y_val, max_255);

            // Calculate U = Ur*R + Ug*G + Ub*B + 128
            let u_r = _mm256_mullo_epi32(r_32, u_kr_v);
            let u_g = _mm256_mullo_epi32(g_32, u_kg_v);
            let u_b = _mm256_mullo_epi32(b_32, u_kb_v);
            let u_sum = _mm256_add_epi32(_mm256_add_epi32(u_r, u_g), u_b);
            let u_sum = _mm256_add_epi32(u_sum, half);
            let u_val = _mm256_srai_epi32(u_sum, 16);
            let u_val = _mm256_add_epi32(u_val, offset_128);
            let u_val = _mm256_max_epi32(u_val, zero);
            let u_val = _mm256_min_epi32(u_val, max_255);

            // Calculate V = Vr*R + Vg*G + Vb*B + 128
            let v_r = _mm256_mullo_epi32(r_32, v_kr_v);
            let v_g = _mm256_mullo_epi32(g_32, v_kg_v);
            let v_b = _mm256_mullo_epi32(b_32, v_kb_v);
            let v_sum = _mm256_add_epi32(_mm256_add_epi32(v_r, v_g), v_b);
            let v_sum = _mm256_add_epi32(v_sum, half);
            let v_val = _mm256_srai_epi32(v_sum, 16);
            let v_val = _mm256_add_epi32(v_val, offset_128);
            let v_val = _mm256_max_epi32(v_val, zero);
            let v_val = _mm256_min_epi32(v_val, max_255);

            // Pack 32-bit values to 8-bit and store
            // Pack 8x i32 -> 8x i16 -> 8x i8
            //
            // AVX2 packing crosses 128-bit lanes, which creates a non-contiguous layout:
            // _mm256_packs_epi32([A0,A1,A2,A3, B0,B1,B2,B3], [C0,C1,C2,C3, D0,D1,D2,D3])
            //   -> [A0,A1,A2,A3, C0,C1,C2,C3, B0,B1,B2,B3, D0,D1,D2,D3]
            //
            // With same input for both args:
            //   -> [L0,L1,L2,L3, L0,L1,L2,L3, H0,H1,H2,H3, H0,H1,H2,H3]
            //
            // After packus_epi16: bytes at [0,1,2,3, 0,1,2,3, 4,5,6,7, 4,5,6,7]
            // We need bytes: [0,1,2,3, 4,5,6,7] from indices [0,1,2,3, 8,9,10,11]

            let y_16 = _mm256_packs_epi32(y_val, y_val);
            let u_16 = _mm256_packs_epi32(u_val, u_val);
            let v_16 = _mm256_packs_epi32(v_val, v_val);

            let y_8 = _mm256_packus_epi16(y_16, y_16);
            let u_8 = _mm256_packus_epi16(u_16, u_16);
            let v_8 = _mm256_packus_epi16(v_16, v_16);

            // Use shuffle to reorder bytes from [0,1,2,3,x,x,x,x,4,5,6,7,x,x,x,x]
            // to [0,1,2,3,4,5,6,7,x,x,x,x,x,x,x,x]
            // Shuffle mask: pick bytes 0,1,2,3,8,9,10,11 and put them in first 8 positions
            let shuffle_mask = _mm_setr_epi8(
                0, 1, 2, 3, 8, 9, 10, 11, // First 8 bytes we want
                -1, -1, -1, -1, -1, -1, -1, -1, // Don't care
            );

            // Extract 128-bit lane and shuffle
            let y_result = _mm_shuffle_epi8(_mm256_castsi256_si128(y_8), shuffle_mask);
            let u_result = _mm_shuffle_epi8(_mm256_castsi256_si128(u_8), shuffle_mask);
            let v_result = _mm_shuffle_epi8(_mm256_castsi256_si128(v_8), shuffle_mask);

            // Store 8 bytes each using storel (stores low 64 bits)
            let y_ptr = frame.y.as_mut_ptr().add(base) as *mut i64;
            let u_ptr = frame.u.as_mut_ptr().add(base) as *mut i64;
            let v_ptr = frame.v.as_mut_ptr().add(base) as *mut i64;

            // _mm_storel_epi64 stores the low 64 bits (8 bytes) to memory
            _mm_storel_epi64(y_ptr as *mut __m128i, y_result);
            _mm_storel_epi64(u_ptr as *mut __m128i, u_result);
            _mm_storel_epi64(v_ptr as *mut __m128i, v_result);
        }

        // Process remaining pixels with scalar code
        if remainder > 0 {
            let start = simd_count * 8;
            let (y_kr, y_kg, y_kb) = matrix.y_coefficients_fixed();
            let (u_kr, u_kg, u_kb) = matrix.u_coefficients_fixed();
            let (v_kr, v_kg, v_kb) = matrix.v_coefficients_fixed();

            for i in start..pixel_count {
                let b = bgra[i * 4] as i32;
                let g = bgra[i * 4 + 1] as i32;
                let r = bgra[i * 4 + 2] as i32;

                let y_val = ((y_kr * r + y_kg * g + y_kb * b + 32768) >> 16).clamp(0, 255);
                let u_val = (((u_kr * r + u_kg * g + u_kb * b + 32768) >> 16) + 128).clamp(0, 255);
                let v_val = (((v_kr * r + v_kg * g + v_kb * b + 32768) >> 16) + 128).clamp(0, 255);

                frame.y[i] = y_val as u8;
                frame.u[i] = u_val as u8;
                frame.v[i] = v_val as u8;
            }
        }
    }
}

/// NEON implementation of BGRA to YUV444 conversion (AArch64)
///
/// Processes 8 pixels per iteration using 128-bit NEON registers.
#[cfg(target_arch = "aarch64")]
fn bgra_to_yuv444_neon(bgra: &[u8], frame: &mut Yuv444Frame, matrix: ColorMatrix) {
    use std::arch::aarch64::*;

    let (y_kr, y_kg, y_kb) = matrix.y_coefficients_fixed();
    let (u_kr, u_kg, u_kb) = matrix.u_coefficients_fixed();
    let (v_kr, v_kg, v_kb) = matrix.v_coefficients_fixed();

    let pixel_count = frame.pixel_count();
    let simd_count = pixel_count / 8;
    let remainder = pixel_count % 8;

    unsafe {
        // Broadcast coefficients
        let y_kr_v = vdupq_n_s32(y_kr);
        let y_kg_v = vdupq_n_s32(y_kg);
        let y_kb_v = vdupq_n_s32(y_kb);

        let u_kr_v = vdupq_n_s32(u_kr);
        let u_kg_v = vdupq_n_s32(u_kg);
        let u_kb_v = vdupq_n_s32(u_kb);

        let v_kr_v = vdupq_n_s32(v_kr);
        let v_kg_v = vdupq_n_s32(v_kg);
        let v_kb_v = vdupq_n_s32(v_kb);

        let half = vdupq_n_s32(32768);
        let offset_128 = vdupq_n_s32(128);

        for i in 0..simd_count {
            let base = i * 8;
            let bgra_offset = base * 4;

            // Load 8 BGRA pixels using vld4 (deinterleave)
            let bgra_ptr = bgra.as_ptr().add(bgra_offset);
            let pixels = vld4_u8(bgra_ptr);

            // pixels.0 = B, pixels.1 = G, pixels.2 = R, pixels.3 = A
            let b_8 = pixels.0;
            let g_8 = pixels.1;
            let r_8 = pixels.2;

            // Extend to 16-bit
            let b_16 = vmovl_u8(b_8);
            let g_16 = vmovl_u8(g_8);
            let r_16 = vmovl_u8(r_8);

            // Process low 4 and high 4 pixels separately
            // Low 4 pixels
            let b_32_lo = vreinterpretq_s32_u32(vmovl_u16(vget_low_u16(b_16)));
            let g_32_lo = vreinterpretq_s32_u32(vmovl_u16(vget_low_u16(g_16)));
            let r_32_lo = vreinterpretq_s32_u32(vmovl_u16(vget_low_u16(r_16)));

            // High 4 pixels
            let b_32_hi = vreinterpretq_s32_u32(vmovl_u16(vget_high_u16(b_16)));
            let g_32_hi = vreinterpretq_s32_u32(vmovl_u16(vget_high_u16(g_16)));
            let r_32_hi = vreinterpretq_s32_u32(vmovl_u16(vget_high_u16(r_16)));

            // Calculate Y for low 4 pixels
            let y_lo = vmulq_s32(r_32_lo, y_kr_v);
            let y_lo = vmlaq_s32(y_lo, g_32_lo, y_kg_v);
            let y_lo = vmlaq_s32(y_lo, b_32_lo, y_kb_v);
            let y_lo = vaddq_s32(y_lo, half);
            let y_lo = vshrq_n_s32(y_lo, 16);
            let y_lo = vmaxq_s32(y_lo, vdupq_n_s32(0));
            let y_lo = vminq_s32(y_lo, vdupq_n_s32(255));

            // Calculate Y for high 4 pixels
            let y_hi = vmulq_s32(r_32_hi, y_kr_v);
            let y_hi = vmlaq_s32(y_hi, g_32_hi, y_kg_v);
            let y_hi = vmlaq_s32(y_hi, b_32_hi, y_kb_v);
            let y_hi = vaddq_s32(y_hi, half);
            let y_hi = vshrq_n_s32(y_hi, 16);
            let y_hi = vmaxq_s32(y_hi, vdupq_n_s32(0));
            let y_hi = vminq_s32(y_hi, vdupq_n_s32(255));

            // Calculate U for low 4 pixels
            let u_lo = vmulq_s32(r_32_lo, u_kr_v);
            let u_lo = vmlaq_s32(u_lo, g_32_lo, u_kg_v);
            let u_lo = vmlaq_s32(u_lo, b_32_lo, u_kb_v);
            let u_lo = vaddq_s32(u_lo, half);
            let u_lo = vshrq_n_s32(u_lo, 16);
            let u_lo = vaddq_s32(u_lo, offset_128);
            let u_lo = vmaxq_s32(u_lo, vdupq_n_s32(0));
            let u_lo = vminq_s32(u_lo, vdupq_n_s32(255));

            // Calculate U for high 4 pixels
            let u_hi = vmulq_s32(r_32_hi, u_kr_v);
            let u_hi = vmlaq_s32(u_hi, g_32_hi, u_kg_v);
            let u_hi = vmlaq_s32(u_hi, b_32_hi, u_kb_v);
            let u_hi = vaddq_s32(u_hi, half);
            let u_hi = vshrq_n_s32(u_hi, 16);
            let u_hi = vaddq_s32(u_hi, offset_128);
            let u_hi = vmaxq_s32(u_hi, vdupq_n_s32(0));
            let u_hi = vminq_s32(u_hi, vdupq_n_s32(255));

            // Calculate V for low 4 pixels
            let v_lo = vmulq_s32(r_32_lo, v_kr_v);
            let v_lo = vmlaq_s32(v_lo, g_32_lo, v_kg_v);
            let v_lo = vmlaq_s32(v_lo, b_32_lo, v_kb_v);
            let v_lo = vaddq_s32(v_lo, half);
            let v_lo = vshrq_n_s32(v_lo, 16);
            let v_lo = vaddq_s32(v_lo, offset_128);
            let v_lo = vmaxq_s32(v_lo, vdupq_n_s32(0));
            let v_lo = vminq_s32(v_lo, vdupq_n_s32(255));

            // Calculate V for high 4 pixels
            let v_hi = vmulq_s32(r_32_hi, v_kr_v);
            let v_hi = vmlaq_s32(v_hi, g_32_hi, v_kg_v);
            let v_hi = vmlaq_s32(v_hi, b_32_hi, v_kb_v);
            let v_hi = vaddq_s32(v_hi, half);
            let v_hi = vshrq_n_s32(v_hi, 16);
            let v_hi = vaddq_s32(v_hi, offset_128);
            let v_hi = vmaxq_s32(v_hi, vdupq_n_s32(0));
            let v_hi = vminq_s32(v_hi, vdupq_n_s32(255));

            // Pack i32 -> i16 -> u8 and store
            let y_16_lo = vmovn_s32(y_lo);
            let y_16_hi = vmovn_s32(y_hi);
            let y_16 = vcombine_s16(y_16_lo, y_16_hi);
            let y_8 = vqmovun_s16(y_16);

            let u_16_lo = vmovn_s32(u_lo);
            let u_16_hi = vmovn_s32(u_hi);
            let u_16 = vcombine_s16(u_16_lo, u_16_hi);
            let u_8 = vqmovun_s16(u_16);

            let v_16_lo = vmovn_s32(v_lo);
            let v_16_hi = vmovn_s32(v_hi);
            let v_16 = vcombine_s16(v_16_lo, v_16_hi);
            let v_8 = vqmovun_s16(v_16);

            // Store results
            vst1_u8(frame.y.as_mut_ptr().add(base), y_8);
            vst1_u8(frame.u.as_mut_ptr().add(base), u_8);
            vst1_u8(frame.v.as_mut_ptr().add(base), v_8);
        }
    }

    // Process remaining pixels with scalar code
    if remainder > 0 {
        let start = simd_count * 8;
        for i in start..pixel_count {
            let b = bgra[i * 4] as i32;
            let g = bgra[i * 4 + 1] as i32;
            let r = bgra[i * 4 + 2] as i32;

            let y_val = ((y_kr * r + y_kg * g + y_kb * b + 32768) >> 16).clamp(0, 255);
            let u_val = (((u_kr * r + u_kg * g + u_kb * b + 32768) >> 16) + 128).clamp(0, 255);
            let v_val = (((v_kr * r + v_kg * g + v_kb * b + 32768) >> 16) + 128).clamp(0, 255);

            frame.y[i] = y_val as u8;
            frame.u[i] = u_val as u8;
            frame.v[i] = v_val as u8;
        }
    }
}

/// Subsample chroma plane from 4:4:4 to 4:2:0 using 2x2 box filter
///
/// This implements proper bilinear filtering for chroma subsampling.
///
/// # Arguments
///
/// * `chroma_444` - Full resolution chroma plane (width x height)
/// * `width` - Original width (must be even)
/// * `height` - Original height (must be even)
///
/// # Returns
///
/// Subsampled chroma plane (width/2 x height/2)
///
/// # Panics
///
/// Panics if width or height is not even
pub fn subsample_chroma_420(chroma_444: &[u8], width: usize, height: usize) -> Vec<u8> {
    assert!(width % 2 == 0, "Width must be even for 4:2:0 subsampling");
    assert!(height % 2 == 0, "Height must be even for 4:2:0 subsampling");
    assert_eq!(
        chroma_444.len(),
        width * height,
        "Chroma plane size mismatch"
    );

    let out_width = width / 2;
    let out_height = height / 2;
    let mut chroma_420 = Vec::with_capacity(out_width * out_height);

    // Dispatch to SIMD if available
    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            // SAFETY: We verified AVX2 is available
            unsafe {
                return subsample_chroma_420_avx2(chroma_444, width, height);
            }
        }
    }

    #[cfg(target_arch = "aarch64")]
    {
        // SAFETY: NEON is always available on AArch64
        unsafe {
            return subsample_chroma_420_neon(chroma_444, width, height);
        }
    }

    // Scalar implementation with proper 2x2 box filter
    for y in (0..height).step_by(2) {
        for x in (0..width).step_by(2) {
            // 2x2 block indices
            let idx00 = y * width + x;
            let idx01 = y * width + (x + 1);
            let idx10 = (y + 1) * width + x;
            let idx11 = (y + 1) * width + (x + 1);

            // Box filter average with proper rounding
            // Add 2 for rounding (divide by 4 with round-half-up)
            let avg = (chroma_444[idx00] as u32
                + chroma_444[idx01] as u32
                + chroma_444[idx10] as u32
                + chroma_444[idx11] as u32
                + 2)
                / 4;

            chroma_420.push(avg as u8);
        }
    }

    chroma_420
}

/// AVX2 implementation of 2x2 box filter chroma subsampling
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn subsample_chroma_420_avx2(chroma_444: &[u8], width: usize, height: usize) -> Vec<u8> {
    unsafe {
        use std::arch::x86_64::{
            __m128i, __m256i, _mm256_add_epi16, _mm256_castsi256_si128, _mm256_hadd_epi16,
            _mm256_loadu_si256, _mm256_packus_epi16, _mm256_permute4x64_epi64, _mm256_set1_epi16,
            _mm256_setzero_si256, _mm256_srli_epi16, _mm256_unpackhi_epi8, _mm256_unpacklo_epi8,
            _mm_storeu_si128,
        };

        let out_width = width / 2;
        let out_height = height / 2;
        let mut chroma_420 = vec![0u8; out_width * out_height];

        // Process 16 output pixels at a time (32 input pixels per row)
        let simd_width = out_width / 16;

        for out_y in 0..out_height {
            let in_y = out_y * 2;
            let row0 = in_y * width;
            let row1 = (in_y + 1) * width;
            let out_row = out_y * out_width;

            for chunk in 0..simd_width {
                let in_x = chunk * 32; // 32 input pixels -> 16 output pixels
                let out_x = chunk * 16;

                // Load 32 pixels from row 0
                let r0_ptr = chroma_444.as_ptr().add(row0 + in_x);
                let r0 = _mm256_loadu_si256(r0_ptr as *const __m256i);

                // Load 32 pixels from row 1
                let r1_ptr = chroma_444.as_ptr().add(row1 + in_x);
                let r1 = _mm256_loadu_si256(r1_ptr as *const __m256i);

                // Add row 0 and row 1 (vertical sum)
                // Need to handle as 16-bit to prevent overflow
                let zero = _mm256_setzero_si256();

                // Unpack to 16-bit
                let r0_lo = _mm256_unpacklo_epi8(r0, zero);
                let r0_hi = _mm256_unpackhi_epi8(r0, zero);
                let r1_lo = _mm256_unpacklo_epi8(r1, zero);
                let r1_hi = _mm256_unpackhi_epi8(r1, zero);

                // Vertical sum
                let v_lo = _mm256_add_epi16(r0_lo, r1_lo);
                let v_hi = _mm256_add_epi16(r0_hi, r1_hi);

                // Horizontal pair sum (add adjacent pixels)
                // Use hadd to sum adjacent 16-bit values
                let h_lo = _mm256_hadd_epi16(v_lo, v_hi);

                // Add rounding constant (2) and divide by 4
                let rounding = _mm256_set1_epi16(2);
                let sum = _mm256_add_epi16(h_lo, rounding);
                let avg = _mm256_srli_epi16(sum, 2);

                // Pack back to 8-bit
                let result = _mm256_packus_epi16(avg, avg);

                // Store 16 output pixels (due to 256-bit lane issues, need to permute)
                let permuted = _mm256_permute4x64_epi64(result, 0b11011000);
                let low_128 = _mm256_castsi256_si128(permuted);

                _mm_storeu_si128(
                    chroma_420.as_mut_ptr().add(out_row + out_x) as *mut __m128i,
                    low_128,
                );
            }

            // Handle remaining pixels with scalar code
            let remaining_start = simd_width * 16;
            for out_x in remaining_start..out_width {
                let in_x = out_x * 2;
                let idx00 = row0 + in_x;
                let idx01 = row0 + in_x + 1;
                let idx10 = row1 + in_x;
                let idx11 = row1 + in_x + 1;

                let avg = (chroma_444[idx00] as u32
                    + chroma_444[idx01] as u32
                    + chroma_444[idx10] as u32
                    + chroma_444[idx11] as u32
                    + 2)
                    / 4;

                chroma_420[out_row + out_x] = avg as u8;
            }
        }

        chroma_420
    }
}

/// NEON implementation of 2x2 box filter chroma subsampling (AArch64)
///
/// Processes 8 output pixels (16 input pixels per row) at a time.
#[cfg(target_arch = "aarch64")]
unsafe fn subsample_chroma_420_neon(chroma_444: &[u8], width: usize, height: usize) -> Vec<u8> {
    use std::arch::aarch64::*;

    let out_width = width / 2;
    let out_height = height / 2;
    let mut chroma_420 = vec![0u8; out_width * out_height];

    // Process 8 output pixels at a time (16 input pixels per row)
    let simd_width = out_width / 8;

    for out_y in 0..out_height {
        let in_y = out_y * 2;
        let row0 = in_y * width;
        let row1 = (in_y + 1) * width;
        let out_row = out_y * out_width;

        for chunk in 0..simd_width {
            let in_x = chunk * 16; // 16 input pixels -> 8 output pixels
            let out_x = chunk * 8;

            // Load 16 pixels from row 0
            let r0 = vld1q_u8(chroma_444.as_ptr().add(row0 + in_x));

            // Load 16 pixels from row 1
            let r1 = vld1q_u8(chroma_444.as_ptr().add(row1 + in_x));

            // Vertical sum (expand to 16-bit to prevent overflow)
            // Low 8 pixels
            let r0_lo = vmovl_u8(vget_low_u8(r0));
            let r1_lo = vmovl_u8(vget_low_u8(r1));
            let v_lo = vaddq_u16(r0_lo, r1_lo);

            // High 8 pixels
            let r0_hi = vmovl_u8(vget_high_u8(r0));
            let r1_hi = vmovl_u8(vget_high_u8(r1));
            let v_hi = vaddq_u16(r0_hi, r1_hi);

            // Horizontal pair sum using pairwise add
            // vpaddq_u16 adds adjacent pairs: [a0+a1, a2+a3, a4+a5, a6+a7, b0+b1, ...]
            let h_sum = vpaddq_u16(v_lo, v_hi);

            // Add rounding constant (2) and divide by 4
            let rounding = vdupq_n_u16(2);
            let sum = vaddq_u16(h_sum, rounding);
            let avg = vshrq_n_u16(sum, 2);

            // Pack back to 8-bit
            let result = vmovn_u16(avg);

            // Store 8 output pixels
            vst1_u8(chroma_420.as_mut_ptr().add(out_row + out_x), result);
        }

        // Handle remaining pixels with scalar code
        let remaining_start = simd_width * 8;
        for out_x in remaining_start..out_width {
            let in_x = out_x * 2;
            let idx00 = row0 + in_x;
            let idx01 = row0 + in_x + 1;
            let idx10 = row1 + in_x;
            let idx11 = row1 + in_x + 1;

            let avg = (chroma_444[idx00] as u32
                + chroma_444[idx01] as u32
                + chroma_444[idx10] as u32
                + chroma_444[idx11] as u32
                + 2)
                / 4;

            chroma_420[out_row + out_x] = avg as u8;
        }
    }

    chroma_420
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_color_matrix_auto_select() {
        // HD resolutions -> BT.709
        assert_eq!(ColorMatrix::auto_select(1920, 1080), ColorMatrix::BT709);
        assert_eq!(ColorMatrix::auto_select(1280, 720), ColorMatrix::BT709);
        assert_eq!(ColorMatrix::auto_select(3840, 2160), ColorMatrix::BT709);

        // SD resolutions -> BT.601
        assert_eq!(ColorMatrix::auto_select(640, 480), ColorMatrix::BT601);
        assert_eq!(ColorMatrix::auto_select(800, 600), ColorMatrix::BT601);
        assert_eq!(ColorMatrix::auto_select(1024, 768), ColorMatrix::BT601);
    }

    #[test]
    fn test_bgra_to_yuv444_white() {
        // White: RGB(255, 255, 255) -> Y=255, U=128, V=128
        let bgra = vec![255, 255, 255, 255]; // 1 white pixel
        let yuv = bgra_to_yuv444(&bgra, 1, 1, ColorMatrix::BT709);

        assert_eq!(yuv.y[0], 255);
        assert!(
            (yuv.u[0] as i32 - 128).abs() <= 1,
            "U should be ~128, got {}",
            yuv.u[0]
        );
        assert!(
            (yuv.v[0] as i32 - 128).abs() <= 1,
            "V should be ~128, got {}",
            yuv.v[0]
        );
    }

    #[test]
    fn test_bgra_to_yuv444_black() {
        // Black: RGB(0, 0, 0) -> Y=0, U=128, V=128
        let bgra = vec![0, 0, 0, 255];
        let yuv = bgra_to_yuv444(&bgra, 1, 1, ColorMatrix::BT709);

        assert_eq!(yuv.y[0], 0);
        assert!(
            (yuv.u[0] as i32 - 128).abs() <= 1,
            "U should be ~128, got {}",
            yuv.u[0]
        );
        assert!(
            (yuv.v[0] as i32 - 128).abs() <= 1,
            "V should be ~128, got {}",
            yuv.v[0]
        );
    }

    #[test]
    fn test_bgra_to_yuv444_pure_red() {
        // Pure red: B=0, G=0, R=255, A=255
        let bgra = vec![0, 0, 255, 255];
        let yuv = bgra_to_yuv444(&bgra, 1, 1, ColorMatrix::BT709);

        // BT.709: Y = 0.2126 * 255 = 54.2
        assert!(
            (yuv.y[0] as i32 - 54).abs() <= 2,
            "Y should be ~54, got {}",
            yuv.y[0]
        );
        // V should be > 128 (shifted toward red)
        assert!(yuv.v[0] > 180, "V should be high for red, got {}", yuv.v[0]);
    }

    #[test]
    fn test_bgra_to_yuv444_pure_green() {
        // Pure green: B=0, G=255, R=0, A=255
        let bgra = vec![0, 255, 0, 255];
        let yuv = bgra_to_yuv444(&bgra, 1, 1, ColorMatrix::BT709);

        // BT.709: Y = 0.7152 * 255 = 182.4
        assert!(
            (yuv.y[0] as i32 - 182).abs() <= 2,
            "Y should be ~182, got {}",
            yuv.y[0]
        );
    }

    #[test]
    fn test_bgra_to_yuv444_pure_blue() {
        // Pure blue: B=255, G=0, R=0, A=255
        let bgra = vec![255, 0, 0, 255];
        let yuv = bgra_to_yuv444(&bgra, 1, 1, ColorMatrix::BT709);

        // BT.709: Y = 0.0722 * 255 = 18.4
        assert!(
            (yuv.y[0] as i32 - 18).abs() <= 2,
            "Y should be ~18, got {}",
            yuv.y[0]
        );
        // U should be > 128 (shifted toward blue)
        assert!(
            yuv.u[0] > 200,
            "U should be high for blue, got {}",
            yuv.u[0]
        );
    }

    #[test]
    fn test_bgra_to_yuv444_bt601_vs_bt709() {
        // Same color should produce different Y values with different matrices
        let bgra = vec![100, 150, 200, 255]; // B=100, G=150, R=200

        let yuv_601 = bgra_to_yuv444(&bgra, 1, 1, ColorMatrix::BT601);
        let yuv_709 = bgra_to_yuv444(&bgra, 1, 1, ColorMatrix::BT709);

        // BT.601 and BT.709 should produce slightly different Y values
        // The difference should be small but measurable
        assert_ne!(
            yuv_601.y[0], yuv_709.y[0],
            "BT.601 and BT.709 should differ"
        );
    }

    #[test]
    fn test_bgra_to_yuv444_larger_frame() {
        // Test 4x4 frame to exercise more code paths
        let width = 4;
        let height = 4;
        let mut bgra = vec![0u8; width * height * 4];

        // Fill with gradient
        for i in 0..(width * height) {
            let val = ((i * 16) % 256) as u8;
            bgra[i * 4] = val; // B
            bgra[i * 4 + 1] = val; // G
            bgra[i * 4 + 2] = val; // R
            bgra[i * 4 + 3] = 255; // A
        }

        let yuv = bgra_to_yuv444(&bgra, width, height, ColorMatrix::BT709);

        assert_eq!(yuv.y.len(), width * height);
        assert_eq!(yuv.u.len(), width * height);
        assert_eq!(yuv.v.len(), width * height);
    }

    #[test]
    fn test_subsample_chroma_420_uniform() {
        // 2x2 block of identical values should average to same value
        let chroma_444 = vec![100, 100, 100, 100];
        let chroma_420 = subsample_chroma_420(&chroma_444, 2, 2);

        assert_eq!(chroma_420.len(), 1);
        assert_eq!(chroma_420[0], 100);
    }

    #[test]
    fn test_subsample_chroma_420_gradient() {
        // 2x2 block: [0, 100, 100, 200] -> avg = (0+100+100+200+2)/4 = 100
        let chroma_444 = vec![0, 100, 100, 200];
        let chroma_420 = subsample_chroma_420(&chroma_444, 2, 2);

        assert_eq!(chroma_420.len(), 1);
        assert_eq!(chroma_420[0], 100);
    }

    #[test]
    fn test_subsample_chroma_420_4x4() {
        // 4x4 input -> 2x2 output
        let chroma_444 = vec![
            10, 20, 30, 40, // Row 0
            50, 60, 70, 80, // Row 1
            90, 100, 110, 120, // Row 2
            130, 140, 150, 160, // Row 3
        ];
        let chroma_420 = subsample_chroma_420(&chroma_444, 4, 4);

        assert_eq!(chroma_420.len(), 4);

        // Block (0,0): (10+20+50+60+2)/4 = 35
        assert_eq!(chroma_420[0], 35);

        // Block (1,0): (30+40+70+80+2)/4 = 55
        assert_eq!(chroma_420[1], 55);

        // Block (0,1): (90+100+130+140+2)/4 = 115
        assert_eq!(chroma_420[2], 115);

        // Block (1,1): (110+120+150+160+2)/4 = 135
        assert_eq!(chroma_420[3], 135);
    }

    #[test]
    #[should_panic(expected = "Width must be even")]
    fn test_subsample_chroma_420_odd_width() {
        let chroma_444 = vec![0; 3 * 2];
        subsample_chroma_420(&chroma_444, 3, 2);
    }

    #[test]
    #[should_panic(expected = "Height must be even")]
    fn test_subsample_chroma_420_odd_height() {
        let chroma_444 = vec![0; 2 * 3];
        subsample_chroma_420(&chroma_444, 2, 3);
    }

    #[test]
    fn test_yuv444_frame_new() {
        let frame = Yuv444Frame::new(1920, 1080);
        assert_eq!(frame.width, 1920);
        assert_eq!(frame.height, 1080);
        assert_eq!(frame.y.len(), 1920 * 1080);
        assert_eq!(frame.u.len(), 1920 * 1080);
        assert_eq!(frame.v.len(), 1920 * 1080);

        // Y should be 0, U/V should be 128 (neutral chroma)
        assert!(frame.y.iter().all(|&v| v == 0));
        assert!(frame.u.iter().all(|&v| v == 128));
        assert!(frame.v.iter().all(|&v| v == 128));
    }

    // =================================================================
    // SIMD-Exercising Tests
    // =================================================================

    /// Test with 8 pixels (minimum SIMD batch size)
    #[test]
    fn test_bgra_to_yuv444_simd_8_pixels() {
        let width = 8;
        let height = 1;
        let mut bgra = vec![0u8; width * height * 4];

        // Create a known pattern: pure white
        for i in 0..8 {
            bgra[i * 4] = 255; // B
            bgra[i * 4 + 1] = 255; // G
            bgra[i * 4 + 2] = 255; // R
            bgra[i * 4 + 3] = 255; // A
        }

        let yuv = bgra_to_yuv444(&bgra, width, height, ColorMatrix::BT709);

        // All Y should be 255, U/V should be 128
        for i in 0..8 {
            assert_eq!(yuv.y[i], 255, "Y[{}] should be 255", i);
            assert!(
                (yuv.u[i] as i32 - 128).abs() <= 1,
                "U[{}] should be ~128",
                i
            );
            assert!(
                (yuv.v[i] as i32 - 128).abs() <= 1,
                "V[{}] should be ~128",
                i
            );
        }
    }

    /// Test with 16 pixels (exercises full AVX2 iteration)
    #[test]
    fn test_bgra_to_yuv444_simd_16_pixels() {
        let width = 16;
        let height = 1;
        let mut bgra = vec![0u8; width * height * 4];

        // Create alternating red/blue pattern
        for i in 0..16 {
            if i % 2 == 0 {
                // Red: B=0, G=0, R=255
                bgra[i * 4] = 0;
                bgra[i * 4 + 1] = 0;
                bgra[i * 4 + 2] = 255;
            } else {
                // Blue: B=255, G=0, R=0
                bgra[i * 4] = 255;
                bgra[i * 4 + 1] = 0;
                bgra[i * 4 + 2] = 0;
            }
            bgra[i * 4 + 3] = 255;
        }

        let yuv = bgra_to_yuv444(&bgra, width, height, ColorMatrix::BT709);

        // Verify pattern was converted correctly
        for i in 0..16 {
            if i % 2 == 0 {
                // Red should have low Y (~54), low U, high V
                assert!(
                    (yuv.y[i] as i32 - 54).abs() <= 2,
                    "Red Y[{}] = {}",
                    i,
                    yuv.y[i]
                );
                assert!(
                    yuv.v[i] > 180,
                    "Red V[{}] = {} should be > 180",
                    i,
                    yuv.v[i]
                );
            } else {
                // Blue should have low Y (~18), high U, low V
                assert!(
                    (yuv.y[i] as i32 - 18).abs() <= 2,
                    "Blue Y[{}] = {}",
                    i,
                    yuv.y[i]
                );
                assert!(
                    yuv.u[i] > 200,
                    "Blue U[{}] = {} should be > 200",
                    i,
                    yuv.u[i]
                );
            }
        }
    }

    /// Test with partial SIMD (8 + remainder)
    #[test]
    fn test_bgra_to_yuv444_simd_with_remainder() {
        let width = 11; // 8 + 3 remainder
        let height = 1;
        let mut bgra = vec![0u8; width * height * 4];

        // Fill with green
        for i in 0..11 {
            bgra[i * 4] = 0; // B
            bgra[i * 4 + 1] = 255; // G
            bgra[i * 4 + 2] = 0; // R
            bgra[i * 4 + 3] = 255; // A
        }

        let yuv = bgra_to_yuv444(&bgra, width, height, ColorMatrix::BT709);

        // All pixels should have same result (green: Y~182)
        for i in 0..11 {
            assert!(
                (yuv.y[i] as i32 - 182).abs() <= 2,
                "Y[{}] = {} should be ~182",
                i,
                yuv.y[i]
            );
        }
    }

    /// Test 1080p conversion (stress test)
    #[test]
    fn test_bgra_to_yuv444_1080p() {
        let width = 1920;
        let height = 1080;
        let mut bgra = vec![0u8; width * height * 4];

        // Create a simple gradient pattern
        for y in 0..height {
            for x in 0..width {
                let idx = (y * width + x) * 4;
                let gray = ((x + y) % 256) as u8;
                bgra[idx] = gray; // B
                bgra[idx + 1] = gray; // G
                bgra[idx + 2] = gray; // R
                bgra[idx + 3] = 255; // A
            }
        }

        let yuv = bgra_to_yuv444(&bgra, width, height, ColorMatrix::BT709);

        assert_eq!(yuv.width, width);
        assert_eq!(yuv.height, height);
        assert_eq!(yuv.y.len(), width * height);

        // Spot check a few values
        // Gray input should produce Y = gray, U = V = 128
        let test_idx = 0;
        let expected_gray = 0u8;
        assert_eq!(yuv.y[test_idx], expected_gray, "First pixel Y mismatch");

        let test_idx2 = width; // Second row, first pixel
        let expected_gray2 = 1u8; // (0 + 1) % 256
        assert_eq!(yuv.y[test_idx2], expected_gray2, "Second row Y mismatch");
    }

    /// Test subsample with 1080p (SIMD stress test)
    #[test]
    fn test_subsample_chroma_420_1080p() {
        let width = 1920;
        let height = 1080;
        let chroma_444 = vec![100u8; width * height];

        let chroma_420 = subsample_chroma_420(&chroma_444, width, height);

        // Output should be half in each dimension
        assert_eq!(chroma_420.len(), 960 * 540);

        // All values should be 100 (uniform input)
        assert!(chroma_420.iter().all(|&v| v == 100));
    }

    /// Test subsample with gradient pattern
    #[test]
    fn test_subsample_chroma_420_gradient_large() {
        let width = 64;
        let height = 64;
        let mut chroma_444 = vec![0u8; width * height];

        // Create gradient
        for y in 0..height {
            for x in 0..width {
                chroma_444[y * width + x] = ((x + y) % 256) as u8;
            }
        }

        let chroma_420 = subsample_chroma_420(&chroma_444, width, height);

        assert_eq!(chroma_420.len(), 32 * 32);

        // Verify 2x2 averaging for first output pixel
        // Input: (0,0)=0, (1,0)=1, (0,1)=1, (1,1)=2
        // Average: (0+1+1+2+2)/4 = 1
        assert_eq!(chroma_420[0], 1);
    }

    /// Test edge values (0 and 255)
    #[test]
    fn test_bgra_to_yuv444_edge_values() {
        // Min values (black)
        let bgra_min = vec![0, 0, 0, 255];
        let yuv_min = bgra_to_yuv444(&bgra_min, 1, 1, ColorMatrix::BT709);
        assert_eq!(yuv_min.y[0], 0);

        // Max values (white)
        let bgra_max = vec![255, 255, 255, 255];
        let yuv_max = bgra_to_yuv444(&bgra_max, 1, 1, ColorMatrix::BT709);
        assert_eq!(yuv_max.y[0], 255);
    }

    /// Test all primary colors in sequence
    #[test]
    fn test_bgra_to_yuv444_rgb_primaries() {
        // Red, Green, Blue, White, Black - 5 pixels
        let bgra = vec![
            0, 0, 255, 255, // Red
            0, 255, 0, 255, // Green
            255, 0, 0, 255, // Blue
            255, 255, 255, 255, // White
            0, 0, 0, 255, // Black
        ];

        let yuv = bgra_to_yuv444(&bgra, 5, 1, ColorMatrix::BT709);

        // BT.709 Y values (approximately):
        // Red:   Y = 0.2126*255 ≈ 54
        // Green: Y = 0.7152*255 ≈ 182
        // Blue:  Y = 0.0722*255 ≈ 18
        // White: Y = 255
        // Black: Y = 0

        assert!((yuv.y[0] as i32 - 54).abs() <= 2, "Red Y = {}", yuv.y[0]);
        assert!((yuv.y[1] as i32 - 182).abs() <= 2, "Green Y = {}", yuv.y[1]);
        assert!((yuv.y[2] as i32 - 18).abs() <= 2, "Blue Y = {}", yuv.y[2]);
        assert_eq!(yuv.y[3], 255, "White Y");
        assert_eq!(yuv.y[4], 0, "Black Y");
    }
}
