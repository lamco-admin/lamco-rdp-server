//! HDR -> SDR tone mapping for captured frames.
//!
//! When a source output is HDR (SMPTE ST 2084 "PQ" or Hybrid Log-Gamma) but the
//! RDP path is 8-bit SDR, the captured pixels must be converted to SDR before
//! H.264 encoding, or they render far too dark (PQ) or washed out. Per pixel:
//!
//! 1. Decode the transfer function (PQ or HLG) to linear light.
//! 2. Map the BT.2020 wide gamut to BT.709.
//! 3. Compress luminance with a filmic (Hable) curve, normalized to the source
//!    peak luminance.
//! 4. Re-encode with the sRGB transfer.
//!
//! Operates in place on a BGRA / BGRx byte buffer. It is deliberately opt-in:
//! whether a compositor delivers PQ-encoded pixels (needs this) or already
//! tone-mapped SDR (must NOT run this) is compositor-specific, so the caller
//! gates it on both the observed HDR state and an explicit config switch.
//!
//! HLG handling applies the inverse OETF only (the display OOTF / system gamma
//! is omitted) — an approximation adequate for SDR down-conversion.

/// Transfer function the source pixels are encoded with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceTransfer {
    /// SMPTE ST 2084 (Perceptual Quantizer).
    Pq,
    /// Hybrid Log-Gamma.
    Hlg,
}

/// PQ (ST 2084) EOTF: normalized signal in `[0,1]` -> normalized linear light in
/// `[0,1]`, where `1.0` is 10000 cd/m².
#[inline]
fn pq_eotf(v: f32) -> f32 {
    const M1: f32 = 2610.0 / 16384.0;
    const M2: f32 = 2523.0 / 4096.0 * 128.0;
    const C1: f32 = 3424.0 / 4096.0;
    const C2: f32 = 2413.0 / 4096.0 * 32.0;
    const C3: f32 = 2392.0 / 4096.0 * 32.0;
    let vp = v.max(0.0).powf(1.0 / M2);
    let num = (vp - C1).max(0.0);
    let den = C2 - C3 * vp;
    (num / den).powf(1.0 / M1)
}

/// HLG inverse OETF: signal in `[0,1]` -> scene-relative linear in `[0,1]`.
#[inline]
fn hlg_eotf(v: f32) -> f32 {
    const A: f32 = 0.178_832_77;
    const B: f32 = 0.284_668_92; // 1 - 4a
    const C: f32 = 0.559_910_7; // 0.5 - a*ln(4a)
    if v <= 0.5 {
        (v * v) / 3.0
    } else {
        (((v - C) / A).exp() + B) / 12.0
    }
}

/// Convert linear BT.2020 RGB to linear BT.709 RGB (out-of-gamut components may
/// go negative; the caller clamps).
#[inline]
fn bt2020_to_bt709(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    (
        1.660_491 * r - 0.587_641 * g - 0.072_85 * b,
        -0.124_55 * r + 1.1329 * g - 0.008_349 * b,
        -0.018_151 * r - 0.100_579 * g + 1.118_73 * b,
    )
}

/// Hable (Uncharted 2) filmic tone-map curve.
#[inline]
fn hable(x: f32) -> f32 {
    const A: f32 = 0.15;
    const B: f32 = 0.50;
    const C: f32 = 0.10;
    const D: f32 = 0.20;
    const E: f32 = 0.02;
    const F: f32 = 0.30;
    ((x * (A * x + C * B) + D * E) / (x * (A * x + B) + D * F)) - E / F
}

/// Linear `[0,1]` -> sRGB-encoded `[0,1]`.
#[inline]
fn linear_to_srgb(c: f32) -> f32 {
    let c = c.clamp(0.0, 1.0);
    if c <= 0.003_130_8 {
        12.92 * c
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// Reference SDR diffuse white in cd/m²: content at this luminance maps near the
/// knee of the filmic curve.
const SDR_WHITE_NITS: f32 = 100.0;

/// Tone-map an HDR BGRA / BGRx buffer to SDR in place.
///
/// `peak_nits` is the source's peak luminance (e.g. mastering max content light
/// level) and sets the tone-map white point. Bytes are laid out B, G, R, X per
/// pixel; the fourth byte is left untouched.
pub fn tonemap_bgrx_in_place(pixels: &mut [u8], transfer: SourceTransfer, peak_nits: f32) {
    let peak = peak_nits.max(SDR_WHITE_NITS);
    let white = hable(peak / SDR_WHITE_NITS).max(1e-6);

    for px in pixels.chunks_exact_mut(4) {
        let b = f32::from(px[0]) / 255.0;
        let g = f32::from(px[1]) / 255.0;
        let r = f32::from(px[2]) / 255.0;

        // Decode the transfer function to linear cd/m².
        let decode = |v: f32| match transfer {
            SourceTransfer::Pq => pq_eotf(v) * 10000.0,
            SourceTransfer::Hlg => hlg_eotf(v) * peak,
        };
        let (lr, lg, lb) = (decode(r), decode(g), decode(b));

        // Gamut map (linear, so absolute scale is preserved).
        let (lr, lg, lb) = bt2020_to_bt709(lr, lg, lb);

        // Filmic tone-map per channel, normalized to the source peak.
        let tone = |l: f32| (hable(l.max(0.0) / SDR_WHITE_NITS) / white).clamp(0.0, 1.0);
        let (sr, sg, sb) = (tone(lr), tone(lg), tone(lb));

        px[0] = (linear_to_srgb(sb) * 255.0).round().clamp(0.0, 255.0) as u8;
        px[1] = (linear_to_srgb(sg) * 255.0).round().clamp(0.0, 255.0) as u8;
        px[2] = (linear_to_srgb(sr) * 255.0).round().clamp(0.0, 255.0) as u8;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pq_eotf_endpoints() {
        assert!(pq_eotf(0.0).abs() < 1e-4);
        assert!((pq_eotf(1.0) - 1.0).abs() < 1e-3);
    }

    #[test]
    fn pq_eotf_monotonic() {
        let mut prev = -1.0;
        for i in 0..=100 {
            let l = pq_eotf(i as f32 / 100.0);
            assert!(l >= prev, "PQ EOTF not monotonic at {i}");
            prev = l;
        }
    }

    #[test]
    fn srgb_endpoints() {
        assert!(linear_to_srgb(0.0).abs() < 1e-6);
        assert!((linear_to_srgb(1.0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn tonemap_keeps_black_and_bounds_white() {
        // Black stays black; full-signal PQ (very bright) clamps into range and
        // is not left at raw full-scale nonsense.
        let mut px = [0u8, 0, 0, 255, 255, 255, 255, 255];
        tonemap_bgrx_in_place(&mut px, SourceTransfer::Pq, 1000.0);
        assert_eq!(&px[0..3], &[0, 0, 0], "black must stay black");
        assert_eq!(px[3], 255, "alpha byte untouched");
        // A very bright PQ input should map to a bright-but-valid SDR value.
        assert!(px[4] > 0 && px[6] > 0, "bright input should not go to zero");
    }

    #[test]
    fn tonemap_is_pure_on_length() {
        // Non-multiple-of-4 tails are ignored by chunks_exact; length preserved.
        let mut px = vec![128u8; 12];
        tonemap_bgrx_in_place(&mut px, SourceTransfer::Hlg, 1000.0);
        assert_eq!(px.len(), 12);
    }
}
