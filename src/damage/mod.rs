//! Damage Region Detection for Bandwidth Optimization
//!
//! This module implements tile-based frame differencing to detect changed screen
//! regions, enabling significant bandwidth reduction (90%+ for static content).
//!
//! # Architecture
//!
//! ```text
//! Current Frame → Tile Grid (64×64) → SIMD Comparison → Dirty Tiles → Merge → DamageRegions
//!                                        vs Previous
//! ```
//!
//! # Algorithm
//!
//! 1. Divide frame into configurable tile grid (default 64×64 pixels)
//! 2. SIMD-compare each tile against previous frame
//! 3. Mark tile dirty if difference exceeds threshold
//! 4. Merge adjacent dirty tiles into larger regions
//! 5. Return optimized list of damage regions
//!
//! # Performance
//!
//! Target: <3ms detection overhead at 1080p resolution
//!
//! # Usage
//!
//! ```rust,ignore
//! use lamco_rdp_server::damage::{DamageDetector, DamageConfig};
//!
//! let config = DamageConfig::default();
//! let mut detector = DamageDetector::new(config);
//!
//! // Process each frame
//! let damage = detector.detect(&frame_data, 1920, 1080);
//!
//! if damage.is_empty() {
//!     // No changes - skip encoding
//! } else {
//!     // Encode only damaged regions
//!     for region in &damage {
//!         encode_region(&frame_data, region);
//!     }
//! }
//! ```

use std::time::Instant;

// =============================================================================
// Types
// =============================================================================

/// A rectangular region of the screen that has changed
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DamageRegion {
    /// X coordinate of the region (pixels from left)
    pub x: u32,
    /// Y coordinate of the region (pixels from top)
    pub y: u32,
    /// Width of the region in pixels
    pub width: u32,
    /// Height of the region in pixels
    pub height: u32,
}

impl DamageRegion {
    /// Create a new damage region
    #[inline]
    pub fn new(x: u32, y: u32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// Create a region covering the entire frame
    #[inline]
    pub fn full_frame(width: u32, height: u32) -> Self {
        Self {
            x: 0,
            y: 0,
            width,
            height,
        }
    }

    /// Calculate the area of this region in pixels
    #[inline]
    pub fn area(&self) -> u64 {
        self.width as u64 * self.height as u64
    }

    /// Check if this region overlaps with another
    pub fn overlaps(&self, other: &DamageRegion) -> bool {
        let self_right = self.x + self.width;
        let self_bottom = self.y + self.height;
        let other_right = other.x + other.width;
        let other_bottom = other.y + other.height;

        self.x < other_right
            && self_right > other.x
            && self.y < other_bottom
            && self_bottom > other.y
    }

    /// Check if this region contains a point
    #[inline]
    pub fn contains(&self, x: u32, y: u32) -> bool {
        x >= self.x && x < self.x + self.width && y >= self.y && y < self.y + self.height
    }

    /// Compute the union (bounding box) of two regions
    pub fn union(&self, other: &DamageRegion) -> DamageRegion {
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        let right = (self.x + self.width).max(other.x + other.width);
        let bottom = (self.y + self.height).max(other.y + other.height);

        DamageRegion {
            x,
            y,
            width: right - x,
            height: bottom - y,
        }
    }

    /// Check if two regions are adjacent (within merge_distance pixels)
    pub fn is_adjacent(&self, other: &DamageRegion, merge_distance: u32) -> bool {
        let self_right = self.x + self.width;
        let self_bottom = self.y + self.height;
        let other_right = other.x + other.width;
        let other_bottom = other.y + other.height;

        // Calculate horizontal gap (0 if overlapping)
        let gap_x = if other.x >= self_right {
            other.x - self_right
        } else {
            self.x.saturating_sub(other_right)
        };

        // Calculate vertical gap (0 if overlapping)
        let gap_y = if other.y >= self_bottom {
            other.y - self_bottom
        } else {
            self.y.saturating_sub(other_bottom)
        };

        // Adjacent if both gaps are within merge_distance
        gap_x <= merge_distance && gap_y <= merge_distance
    }
}

/// Configuration for damage detection
#[derive(Debug, Clone)]
pub struct DamageConfig {
    /// Size of each comparison tile in pixels (default: 64)
    pub tile_size: usize,

    /// Fraction of tile pixels that must differ to mark as dirty (default: 0.05 = 5%)
    pub diff_threshold: f32,

    /// Maximum pixel difference to consider "same" (default: 4)
    /// Pixels differing by less than this are considered identical
    pub pixel_threshold: u8,

    /// Distance in pixels for merging adjacent dirty tiles (default: 32)
    pub merge_distance: u32,

    /// Minimum region area to report (default: 256 = 16×16)
    /// Regions smaller than this are merged or ignored
    pub min_region_area: u64,
}

impl Default for DamageConfig {
    fn default() -> Self {
        Self {
            tile_size: 64,
            diff_threshold: 0.05,
            pixel_threshold: 4,
            merge_distance: 32,
            min_region_area: 256,
        }
    }
}

impl DamageConfig {
    /// Create config optimized for low-bandwidth scenarios
    pub fn low_bandwidth() -> Self {
        Self {
            tile_size: 32,        // Finer granularity
            diff_threshold: 0.02, // More sensitive
            pixel_threshold: 2,
            merge_distance: 16,
            min_region_area: 64,
        }
    }

    /// Create config optimized for high-motion content
    pub fn high_motion() -> Self {
        Self {
            tile_size: 128,       // Coarser for speed
            diff_threshold: 0.10, // Less sensitive
            pixel_threshold: 8,
            merge_distance: 64,
            min_region_area: 1024,
        }
    }
}

/// Statistics about damage detection performance
#[derive(Debug, Clone, Default)]
pub struct DamageStats {
    /// Total frames processed
    pub frames_processed: u64,

    /// Frames with no damage (completely static)
    pub frames_skipped: u64,

    /// Frames with full-frame damage
    pub frames_full: u64,

    /// Frames with partial damage
    pub frames_partial: u64,

    /// Total damaged area across all frames (pixels)
    pub total_damage_area: u64,

    /// Total frame area across all frames (pixels)
    pub total_frame_area: u64,

    /// Total time spent on detection (nanoseconds)
    pub total_detection_time_ns: u64,

    /// Average damage ratio (damaged_area / frame_area)
    pub avg_damage_ratio: f32,

    /// Average detection time in milliseconds
    pub avg_detection_time_ms: f32,
}

impl DamageStats {
    /// Calculate bandwidth reduction percentage
    pub fn bandwidth_reduction_percent(&self) -> f32 {
        if self.total_frame_area == 0 {
            return 0.0;
        }
        let ratio = self.total_damage_area as f32 / self.total_frame_area as f32;
        (1.0 - ratio) * 100.0
    }

    fn update_averages(&mut self) {
        if self.frames_processed > 0 {
            self.avg_damage_ratio =
                self.total_damage_area as f32 / self.total_frame_area.max(1) as f32;
            self.avg_detection_time_ms = (self.total_detection_time_ns as f64
                / self.frames_processed as f64
                / 1_000_000.0) as f32;
        }
    }
}

// =============================================================================
// SIMD Tile Comparison
// =============================================================================

/// Count pixels that differ by more than threshold (scalar fallback)
fn count_different_pixels_scalar(prev: &[u8], curr: &[u8], threshold: u8) -> u32 {
    let mut count = 0u32;

    // Process 4 bytes at a time (BGRA pixels)
    for (p, c) in prev.chunks_exact(4).zip(curr.chunks_exact(4)) {
        // Check if any channel differs by more than threshold
        let diff_b = (p[0] as i16 - c[0] as i16).unsigned_abs() as u8;
        let diff_g = (p[1] as i16 - c[1] as i16).unsigned_abs() as u8;
        let diff_r = (p[2] as i16 - c[2] as i16).unsigned_abs() as u8;
        // Skip alpha channel (index 3)

        if diff_b > threshold || diff_g > threshold || diff_r > threshold {
            count += 1;
        }
    }

    count
}

/// SIMD-optimized tile comparison for x86_64 with AVX2
#[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
fn count_different_pixels_avx2(prev: &[u8], curr: &[u8], threshold: u8) -> u32 {
    use std::arch::x86_64::*;

    // Fall back to scalar for small buffers
    if prev.len() < 32 || curr.len() < 32 {
        return count_different_pixels_scalar(prev, curr, threshold);
    }

    unsafe {
        let threshold_vec = _mm256_set1_epi8(threshold as i8);
        let mut diff_count = 0u32;

        // Process 32 bytes (8 BGRA pixels) at a time
        let chunks = prev.len() / 32;

        for i in 0..chunks {
            let offset = i * 32;
            let prev_ptr = prev.as_ptr().add(offset) as *const __m256i;
            let curr_ptr = curr.as_ptr().add(offset) as *const __m256i;

            let prev_data = _mm256_loadu_si256(prev_ptr);
            let curr_data = _mm256_loadu_si256(curr_ptr);

            // Compute absolute difference
            let diff = _mm256_or_si256(
                _mm256_subs_epu8(prev_data, curr_data),
                _mm256_subs_epu8(curr_data, prev_data),
            );

            // Compare against threshold
            let exceeds = _mm256_cmpgt_epi8(diff, threshold_vec);

            // Count lanes that exceed threshold
            let mask = _mm256_movemask_epi8(exceeds) as u32;
            diff_count += mask.count_ones();
        }

        // Process remaining bytes with scalar
        let remaining_start = chunks * 32;
        if remaining_start < prev.len() {
            diff_count += count_different_pixels_scalar(
                &prev[remaining_start..],
                &curr[remaining_start..],
                threshold,
            );
        }

        // Convert byte count to pixel count (4 bytes per pixel, but we're checking RGB only)
        // The mask gives us byte-level differences, divide by 4 for approximate pixel count
        diff_count / 3
    }
}

/// SIMD-optimized tile comparison for aarch64 with NEON
#[cfg(all(target_arch = "aarch64", target_feature = "neon"))]
fn count_different_pixels_neon(prev: &[u8], curr: &[u8], threshold: u8) -> u32 {
    use std::arch::aarch64::*;

    // Fall back to scalar for small buffers
    if prev.len() < 16 || curr.len() < 16 {
        return count_different_pixels_scalar(prev, curr, threshold);
    }

    unsafe {
        let threshold_vec = vdupq_n_u8(threshold);
        let mut diff_count = 0u32;

        // Process 16 bytes (4 BGRA pixels) at a time
        let chunks = prev.len() / 16;

        for i in 0..chunks {
            let offset = i * 16;
            let prev_data = vld1q_u8(prev.as_ptr().add(offset));
            let curr_data = vld1q_u8(curr.as_ptr().add(offset));

            // Compute absolute difference
            let diff = vabdq_u8(prev_data, curr_data);

            // Compare against threshold
            let exceeds = vcgtq_u8(diff, threshold_vec);

            // Horizontal sum of comparison results (count lanes that exceed)
            // Each lane is 0xFF if exceeds, 0x00 if not
            let sum = vaddvq_u8(exceeds);
            diff_count += (sum / 255) as u32;
        }

        // Process remaining bytes with scalar
        let remaining_start = chunks * 16;
        if remaining_start < prev.len() {
            diff_count += count_different_pixels_scalar(
                &prev[remaining_start..],
                &curr[remaining_start..],
                threshold,
            );
        }

        // Convert to approximate pixel count
        diff_count / 3
    }
}

/// Count different pixels using the best available SIMD implementation
#[inline]
fn count_different_pixels(prev: &[u8], curr: &[u8], threshold: u8) -> u32 {
    #[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
    {
        count_different_pixels_avx2(prev, curr, threshold)
    }

    #[cfg(all(target_arch = "aarch64", target_feature = "neon"))]
    {
        count_different_pixels_neon(prev, curr, threshold)
    }

    #[cfg(not(any(
        all(target_arch = "x86_64", target_feature = "avx2"),
        all(target_arch = "aarch64", target_feature = "neon")
    )))]
    {
        count_different_pixels_scalar(prev, curr, threshold)
    }
}

// =============================================================================
// Region Merging
// =============================================================================

/// Merge adjacent regions that are within merge_distance of each other
fn merge_regions(mut regions: Vec<DamageRegion>, merge_distance: u32) -> Vec<DamageRegion> {
    if regions.len() <= 1 {
        return regions;
    }

    // Iteratively merge until no more merges possible
    let mut changed = true;
    while changed {
        changed = false;
        let mut merged = Vec::with_capacity(regions.len());
        let mut used = vec![false; regions.len()];

        for i in 0..regions.len() {
            if used[i] {
                continue;
            }

            let mut current = regions[i];
            used[i] = true;

            // Find all regions that can be merged with current
            for j in (i + 1)..regions.len() {
                if used[j] {
                    continue;
                }

                if current.is_adjacent(&regions[j], merge_distance) {
                    current = current.union(&regions[j]);
                    used[j] = true;
                    changed = true;
                }
            }

            merged.push(current);
        }

        regions = merged;
    }

    regions
}

/// Convert dirty tiles to damage regions
fn tiles_to_regions(
    dirty_tiles: &[bool],
    tiles_x: usize,
    tiles_y: usize,
    tile_size: usize,
    frame_width: u32,
    frame_height: u32,
) -> Vec<DamageRegion> {
    let mut regions = Vec::new();

    for ty in 0..tiles_y {
        for tx in 0..tiles_x {
            let idx = ty * tiles_x + tx;
            if dirty_tiles[idx] {
                // Calculate tile bounds, clamped to frame dimensions
                let x = (tx * tile_size) as u32;
                let y = (ty * tile_size) as u32;
                let width = (tile_size as u32).min(frame_width.saturating_sub(x));
                let height = (tile_size as u32).min(frame_height.saturating_sub(y));

                if width > 0 && height > 0 {
                    regions.push(DamageRegion::new(x, y, width, height));
                }
            }
        }
    }

    regions
}

// =============================================================================
// DamageDetector
// =============================================================================

/// Main damage detection engine
///
/// Compares consecutive frames to identify changed regions,
/// enabling bandwidth-efficient encoding of only modified areas.
pub struct DamageDetector {
    /// Configuration settings
    config: DamageConfig,

    /// Previous frame data for comparison
    previous_frame: Option<Vec<u8>>,

    /// Previous frame dimensions
    previous_dimensions: Option<(u32, u32)>,

    /// Dirty tile grid (reused between frames)
    tile_dirty: Vec<bool>,

    /// Number of tiles horizontally
    tiles_x: usize,

    /// Number of tiles vertically
    tiles_y: usize,

    /// Detection statistics
    stats: DamageStats,

    /// Force full-frame on next detection
    invalidated: bool,
}

impl DamageDetector {
    /// Create a new damage detector with the given configuration
    pub fn new(config: DamageConfig) -> Self {
        Self {
            config,
            previous_frame: None,
            previous_dimensions: None,
            tile_dirty: Vec::new(),
            tiles_x: 0,
            tiles_y: 0,
            stats: DamageStats::default(),
            invalidated: true,
        }
    }

    /// Create a detector with default configuration
    pub fn with_defaults() -> Self {
        Self::new(DamageConfig::default())
    }

    /// Detect damaged regions in the current frame
    ///
    /// Returns a list of regions that have changed since the previous frame.
    /// Returns an empty vector if the frame is identical to the previous one.
    /// Returns full-frame damage on the first frame or after invalidation.
    ///
    /// # Arguments
    ///
    /// * `frame` - BGRA pixel data (4 bytes per pixel)
    /// * `width` - Frame width in pixels
    /// * `height` - Frame height in pixels
    ///
    /// # Panics
    ///
    /// Panics if frame length doesn't match width * height * 4
    pub fn detect(&mut self, frame: &[u8], width: u32, height: u32) -> Vec<DamageRegion> {
        let start = Instant::now();
        let frame_area = width as u64 * height as u64;
        let expected_len = (width as usize) * (height as usize) * 4;

        assert_eq!(
            frame.len(),
            expected_len,
            "Frame size mismatch: got {} bytes, expected {} for {}×{}",
            frame.len(),
            expected_len,
            width,
            height
        );

        // Check for dimension change
        let dimensions_changed = self
            .previous_dimensions
            .map(|(w, h)| w != width || h != height)
            .unwrap_or(true);

        // Handle first frame, invalidation, or dimension change
        if self.previous_frame.is_none() || self.invalidated || dimensions_changed {
            self.update_tile_grid(width, height);
            self.previous_frame = Some(frame.to_vec());
            self.previous_dimensions = Some((width, height));
            self.invalidated = false;

            // Record stats
            self.stats.frames_processed += 1;
            self.stats.frames_full += 1;
            self.stats.total_damage_area += frame_area;
            self.stats.total_frame_area += frame_area;
            self.stats.total_detection_time_ns += start.elapsed().as_nanos() as u64;
            self.stats.update_averages();

            return vec![DamageRegion::full_frame(width, height)];
        }

        // Take ownership of previous frame temporarily to avoid borrow issues
        let mut prev_frame = self.previous_frame.take().unwrap();
        let regions = self.detect_changes(&prev_frame, frame, width, height);

        // Calculate damage area
        let damage_area: u64 = regions.iter().map(|r| r.area()).sum();

        // Update stats
        self.stats.frames_processed += 1;
        self.stats.total_damage_area += damage_area;
        self.stats.total_frame_area += frame_area;

        if regions.is_empty() {
            self.stats.frames_skipped += 1;
        } else if damage_area >= frame_area * 9 / 10 {
            // 90%+ damage = effectively full frame
            self.stats.frames_full += 1;
        } else {
            self.stats.frames_partial += 1;
        }

        self.stats.total_detection_time_ns += start.elapsed().as_nanos() as u64;
        self.stats.update_averages();

        // Store current frame for next comparison (reuse allocation)
        prev_frame.clear();
        prev_frame.extend_from_slice(frame);
        self.previous_frame = Some(prev_frame);

        regions
    }

    /// Force full-frame damage on the next detect() call
    ///
    /// Call this after resolution changes, keyframe requests,
    /// or other events that require a full refresh.
    pub fn invalidate(&mut self) {
        self.invalidated = true;
    }

    /// Get current detection statistics
    pub fn stats(&self) -> &DamageStats {
        &self.stats
    }

    /// Reset statistics to zero
    pub fn reset_stats(&mut self) {
        self.stats = DamageStats::default();
    }

    /// Get the current configuration
    pub fn config(&self) -> &DamageConfig {
        &self.config
    }

    /// Update configuration
    ///
    /// Note: This invalidates the detector, causing the next frame
    /// to be treated as full damage.
    pub fn set_config(&mut self, config: DamageConfig) {
        self.config = config;
        self.invalidate();
    }

    // -------------------------------------------------------------------------
    // Internal methods
    // -------------------------------------------------------------------------

    fn update_tile_grid(&mut self, width: u32, height: u32) {
        self.tiles_x = (width as usize).div_ceil(self.config.tile_size);
        self.tiles_y = (height as usize).div_ceil(self.config.tile_size);
        let total_tiles = self.tiles_x * self.tiles_y;

        if self.tile_dirty.len() != total_tiles {
            self.tile_dirty = vec![false; total_tiles];
        }
    }

    fn detect_changes(
        &mut self,
        prev: &[u8],
        curr: &[u8],
        width: u32,
        height: u32,
    ) -> Vec<DamageRegion> {
        let tile_size = self.config.tile_size;
        let stride = (width as usize) * 4;
        let pixel_threshold = self.config.pixel_threshold;
        let tile_pixels = (tile_size * tile_size) as u32;
        let diff_threshold_count = (tile_pixels as f32 * self.config.diff_threshold) as u32;

        // Reset dirty flags
        for flag in &mut self.tile_dirty {
            *flag = false;
        }

        // Compare each tile
        for ty in 0..self.tiles_y {
            for tx in 0..self.tiles_x {
                let tile_x = tx * tile_size;
                let tile_y = ty * tile_size;

                // Calculate actual tile dimensions (may be smaller at edges)
                let tile_width = tile_size.min((width as usize).saturating_sub(tile_x));
                let tile_height = tile_size.min((height as usize).saturating_sub(tile_y));

                if tile_width == 0 || tile_height == 0 {
                    continue;
                }

                // Compare tile contents
                let diff_count = self.compare_tile(
                    prev,
                    curr,
                    tile_x,
                    tile_y,
                    tile_width,
                    tile_height,
                    stride,
                    pixel_threshold,
                );

                // Mark dirty if difference exceeds threshold
                let idx = ty * self.tiles_x + tx;
                self.tile_dirty[idx] = diff_count > diff_threshold_count;
            }
        }

        // Convert dirty tiles to regions
        let mut regions = tiles_to_regions(
            &self.tile_dirty,
            self.tiles_x,
            self.tiles_y,
            tile_size,
            width,
            height,
        );

        // Merge adjacent regions
        regions = merge_regions(regions, self.config.merge_distance);

        // Filter out tiny regions
        regions.retain(|r| r.area() >= self.config.min_region_area);

        regions
    }

    fn compare_tile(
        &self,
        prev: &[u8],
        curr: &[u8],
        tile_x: usize,
        tile_y: usize,
        tile_width: usize,
        tile_height: usize,
        stride: usize,
        pixel_threshold: u8,
    ) -> u32 {
        let mut total_diff = 0u32;
        let bytes_per_row = tile_width * 4;

        for row in 0..tile_height {
            let y = tile_y + row;
            let offset = y * stride + tile_x * 4;

            // Bounds check
            if offset + bytes_per_row > prev.len() || offset + bytes_per_row > curr.len() {
                continue;
            }

            let prev_row = &prev[offset..offset + bytes_per_row];
            let curr_row = &curr[offset..offset + bytes_per_row];

            total_diff += count_different_pixels(prev_row, curr_row, pixel_threshold);
        }

        total_diff
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn create_solid_frame(width: usize, height: usize, color: [u8; 4]) -> Vec<u8> {
        let mut data = vec![0u8; width * height * 4];
        for pixel in data.chunks_exact_mut(4) {
            pixel.copy_from_slice(&color);
        }
        data
    }

    fn create_frame_with_region(
        width: usize,
        height: usize,
        bg_color: [u8; 4],
        region: DamageRegion,
        region_color: [u8; 4],
    ) -> Vec<u8> {
        let mut data = create_solid_frame(width, height, bg_color);

        for y in region.y..(region.y + region.height) {
            for x in region.x..(region.x + region.width) {
                if (x as usize) < width && (y as usize) < height {
                    let idx = ((y as usize) * width + (x as usize)) * 4;
                    data[idx..idx + 4].copy_from_slice(&region_color);
                }
            }
        }

        data
    }

    // -------------------------------------------------------------------------
    // DamageRegion tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_damage_region_area() {
        let region = DamageRegion::new(0, 0, 100, 50);
        assert_eq!(region.area(), 5000);
    }

    #[test]
    fn test_damage_region_full_frame() {
        let region = DamageRegion::full_frame(1920, 1080);
        assert_eq!(region.x, 0);
        assert_eq!(region.y, 0);
        assert_eq!(region.width, 1920);
        assert_eq!(region.height, 1080);
    }

    #[test]
    fn test_damage_region_overlaps() {
        let r1 = DamageRegion::new(0, 0, 100, 100);
        let r2 = DamageRegion::new(50, 50, 100, 100);
        let r3 = DamageRegion::new(200, 200, 100, 100);

        assert!(r1.overlaps(&r2));
        assert!(r2.overlaps(&r1));
        assert!(!r1.overlaps(&r3));
        assert!(!r3.overlaps(&r1));
    }

    #[test]
    fn test_damage_region_contains() {
        let region = DamageRegion::new(10, 20, 100, 50);
        assert!(region.contains(10, 20)); // Top-left
        assert!(region.contains(50, 40)); // Inside
        assert!(!region.contains(9, 20)); // Just outside left
        assert!(!region.contains(110, 20)); // Just outside right
    }

    #[test]
    fn test_damage_region_union() {
        let r1 = DamageRegion::new(0, 0, 50, 50);
        let r2 = DamageRegion::new(30, 30, 50, 50);
        let union = r1.union(&r2);

        assert_eq!(union.x, 0);
        assert_eq!(union.y, 0);
        assert_eq!(union.width, 80);
        assert_eq!(union.height, 80);
    }

    #[test]
    fn test_damage_region_is_adjacent() {
        let r1 = DamageRegion::new(0, 0, 64, 64);
        let r2 = DamageRegion::new(80, 0, 64, 64); // 16 pixels gap
        let r3 = DamageRegion::new(200, 0, 64, 64); // Far away

        assert!(r1.is_adjacent(&r2, 32)); // 32px merge distance covers gap
        assert!(!r1.is_adjacent(&r2, 10)); // 10px merge distance doesn't
        assert!(!r1.is_adjacent(&r3, 32));
    }

    // -------------------------------------------------------------------------
    // DamageConfig tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_damage_config_default() {
        let config = DamageConfig::default();
        assert_eq!(config.tile_size, 64);
        assert!((config.diff_threshold - 0.05).abs() < 0.001);
        assert_eq!(config.pixel_threshold, 4);
        assert_eq!(config.merge_distance, 32);
    }

    #[test]
    fn test_damage_config_presets() {
        let low_bw = DamageConfig::low_bandwidth();
        assert_eq!(low_bw.tile_size, 32);
        assert!(low_bw.diff_threshold < 0.05);

        let high_motion = DamageConfig::high_motion();
        assert_eq!(high_motion.tile_size, 128);
        assert!(high_motion.diff_threshold > 0.05);
    }

    // -------------------------------------------------------------------------
    // Pixel comparison tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_count_different_pixels_identical() {
        let data = vec![100u8; 64];
        let count = count_different_pixels_scalar(&data, &data, 4);
        assert_eq!(count, 0);
    }

    #[test]
    fn test_count_different_pixels_all_different() {
        let prev = vec![0u8; 64];
        let curr = vec![255u8; 64];
        let count = count_different_pixels_scalar(&prev, &curr, 4);
        assert_eq!(count, 16); // 64 bytes / 4 bytes per pixel
    }

    #[test]
    fn test_count_different_pixels_threshold() {
        let prev = vec![100u8; 64];
        let mut curr = prev.clone();

        // Change first pixel slightly (within threshold)
        curr[0] = 103; // Diff of 3
        let count = count_different_pixels_scalar(&prev, &curr, 4);
        assert_eq!(count, 0); // Below threshold

        // Change first pixel more (exceeds threshold)
        curr[0] = 110; // Diff of 10
        let count = count_different_pixels_scalar(&prev, &curr, 4);
        assert_eq!(count, 1); // Above threshold
    }

    // -------------------------------------------------------------------------
    // Region merging tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_merge_regions_empty() {
        let regions = merge_regions(vec![], 32);
        assert!(regions.is_empty());
    }

    #[test]
    fn test_merge_regions_single() {
        let region = DamageRegion::new(0, 0, 64, 64);
        let regions = merge_regions(vec![region], 32);
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0], region);
    }

    #[test]
    fn test_merge_regions_adjacent() {
        let r1 = DamageRegion::new(0, 0, 64, 64);
        let r2 = DamageRegion::new(64, 0, 64, 64); // Adjacent

        let regions = merge_regions(vec![r1, r2], 32);
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].width, 128);
    }

    #[test]
    fn test_merge_regions_separate() {
        let r1 = DamageRegion::new(0, 0, 64, 64);
        let r2 = DamageRegion::new(200, 200, 64, 64); // Far apart

        let regions = merge_regions(vec![r1, r2], 32);
        assert_eq!(regions.len(), 2);
    }

    #[test]
    fn test_merge_regions_chain() {
        // Three regions in a chain: A-B-C where A adjacent to B, B adjacent to C
        let r1 = DamageRegion::new(0, 0, 64, 64);
        let r2 = DamageRegion::new(80, 0, 64, 64);
        let r3 = DamageRegion::new(160, 0, 64, 64);

        let regions = merge_regions(vec![r1, r2, r3], 32);
        assert_eq!(regions.len(), 1); // All merged into one
        assert_eq!(regions[0].width, 224);
    }

    // -------------------------------------------------------------------------
    // DamageDetector tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_detector_first_frame_full_damage() {
        let mut detector = DamageDetector::with_defaults();
        let frame = create_solid_frame(640, 480, [0, 0, 0, 255]);

        let damage = detector.detect(&frame, 640, 480);

        assert_eq!(damage.len(), 1);
        assert_eq!(damage[0], DamageRegion::full_frame(640, 480));
    }

    #[test]
    fn test_detector_identical_frames_no_damage() {
        let mut detector = DamageDetector::with_defaults();
        let frame = create_solid_frame(640, 480, [100, 100, 100, 255]);

        // First frame
        let _ = detector.detect(&frame, 640, 480);

        // Second identical frame
        let damage = detector.detect(&frame, 640, 480);
        assert!(damage.is_empty(), "Identical frames should have no damage");
    }

    #[test]
    fn test_detector_partial_change() {
        let mut detector = DamageDetector::new(DamageConfig {
            tile_size: 64,
            diff_threshold: 0.01, // Very sensitive
            pixel_threshold: 1,
            merge_distance: 0, // No merging
            min_region_area: 1,
        });

        let frame1 = create_solid_frame(256, 256, [0, 0, 0, 255]);

        // Create frame with a changed region in top-left corner
        let changed_region = DamageRegion::new(0, 0, 64, 64);
        let frame2 = create_frame_with_region(
            256,
            256,
            [0, 0, 0, 255],
            changed_region,
            [255, 255, 255, 255],
        );

        // First frame
        let _ = detector.detect(&frame1, 256, 256);

        // Second frame with partial change
        let damage = detector.detect(&frame2, 256, 256);

        assert!(!damage.is_empty(), "Should detect damage");

        // Check that damage is in the expected area
        let total_damage_area: u64 = damage.iter().map(|r| r.area()).sum();
        let expected_area = changed_region.area();
        assert!(
            total_damage_area >= expected_area / 2,
            "Damage area {} should include changed region {}",
            total_damage_area,
            expected_area
        );
    }

    #[test]
    fn test_detector_dimension_change_invalidates() {
        let mut detector = DamageDetector::with_defaults();

        let frame1 = create_solid_frame(640, 480, [100, 100, 100, 255]);
        let frame2 = create_solid_frame(800, 600, [100, 100, 100, 255]);

        // First frame at 640x480
        let damage1 = detector.detect(&frame1, 640, 480);
        assert_eq!(damage1[0], DamageRegion::full_frame(640, 480));

        // Second frame at different resolution
        let damage2 = detector.detect(&frame2, 800, 600);
        assert_eq!(damage2.len(), 1);
        assert_eq!(damage2[0], DamageRegion::full_frame(800, 600));
    }

    #[test]
    fn test_detector_invalidate() {
        let mut detector = DamageDetector::with_defaults();
        let frame = create_solid_frame(640, 480, [100, 100, 100, 255]);

        // First frame
        let _ = detector.detect(&frame, 640, 480);

        // Invalidate
        detector.invalidate();

        // Should get full damage again
        let damage = detector.detect(&frame, 640, 480);
        assert_eq!(damage.len(), 1);
        assert_eq!(damage[0], DamageRegion::full_frame(640, 480));
    }

    #[test]
    fn test_detector_stats() {
        let mut detector = DamageDetector::with_defaults();
        let frame = create_solid_frame(640, 480, [0, 0, 0, 255]);

        // Process several frames
        for _ in 0..5 {
            let _ = detector.detect(&frame, 640, 480);
        }

        let stats = detector.stats();
        assert_eq!(stats.frames_processed, 5);
        assert_eq!(stats.frames_full, 1); // First frame only
        assert_eq!(stats.frames_skipped, 4); // Identical frames
        assert!(stats.bandwidth_reduction_percent() > 0.0);
    }

    #[test]
    fn test_detector_config_update() {
        let mut detector = DamageDetector::with_defaults();
        let frame = create_solid_frame(640, 480, [100, 100, 100, 255]);

        // First frame
        let _ = detector.detect(&frame, 640, 480);

        // Update config
        detector.set_config(DamageConfig::high_motion());

        // Should invalidate and return full damage
        let damage = detector.detect(&frame, 640, 480);
        assert_eq!(damage.len(), 1);
        assert_eq!(damage[0], DamageRegion::full_frame(640, 480));
    }

    // -------------------------------------------------------------------------
    // Edge case tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_detector_odd_dimensions() {
        let mut detector = DamageDetector::with_defaults();
        let frame = create_solid_frame(641, 479, [128, 128, 128, 255]); // Odd dimensions

        let damage = detector.detect(&frame, 641, 479);
        assert_eq!(damage.len(), 1);
        assert_eq!(damage[0], DamageRegion::full_frame(641, 479));

        // Second frame should work too
        let damage2 = detector.detect(&frame, 641, 479);
        assert!(damage2.is_empty());
    }

    #[test]
    fn test_detector_small_frame() {
        let mut detector = DamageDetector::new(DamageConfig {
            tile_size: 64,
            min_region_area: 1,
            ..Default::default()
        });
        let frame = create_solid_frame(32, 32, [50, 50, 50, 255]); // Smaller than tile

        let damage = detector.detect(&frame, 32, 32);
        assert_eq!(damage.len(), 1);
        assert_eq!(damage[0].area(), 32 * 32);
    }

    #[test]
    fn test_detector_large_frame() {
        let mut detector = DamageDetector::with_defaults();
        let frame = create_solid_frame(3840, 2160, [0, 128, 255, 255]); // 4K

        let damage = detector.detect(&frame, 3840, 2160);
        assert_eq!(damage.len(), 1);
        assert_eq!(damage[0], DamageRegion::full_frame(3840, 2160));

        // Identical second frame
        let damage2 = detector.detect(&frame, 3840, 2160);
        assert!(damage2.is_empty());
    }

    #[test]
    #[should_panic(expected = "Frame size mismatch")]
    fn test_detector_wrong_size_panics() {
        let mut detector = DamageDetector::with_defaults();
        let frame = create_solid_frame(640, 480, [0, 0, 0, 255]);

        // Pass wrong dimensions
        let _ = detector.detect(&frame, 800, 600);
    }

    // -------------------------------------------------------------------------
    // Tiles to regions tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_tiles_to_regions_single() {
        let mut dirty = vec![false; 16]; // 4×4 grid
        dirty[5] = true; // (1, 1) tile

        let regions = tiles_to_regions(&dirty, 4, 4, 64, 256, 256);
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].x, 64);
        assert_eq!(regions[0].y, 64);
        assert_eq!(regions[0].width, 64);
        assert_eq!(regions[0].height, 64);
    }

    #[test]
    fn test_tiles_to_regions_edge_clamping() {
        let mut dirty = vec![false; 4]; // 2×2 grid
        dirty[3] = true; // Bottom-right tile

        // Frame is 100×100 with 64px tiles
        // Bottom-right tile should be clamped to (64, 64, 36, 36)
        let regions = tiles_to_regions(&dirty, 2, 2, 64, 100, 100);
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].x, 64);
        assert_eq!(regions[0].y, 64);
        assert_eq!(regions[0].width, 36);
        assert_eq!(regions[0].height, 36);
    }
}
