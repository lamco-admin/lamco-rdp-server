//! Damage Detection Benchmarks
//!
//! Measures performance of tile-based frame comparison and region detection
//! at various resolutions.

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use lamco_rdp_server::damage::{DamageConfig, DamageDetector, DamageRegion};

/// Generate test BGRA data with a gradient pattern
fn generate_bgra_frame(width: usize, height: usize, offset: usize) -> Vec<u8> {
    let mut data = vec![0u8; width * height * 4];
    for y in 0..height {
        for x in 0..width {
            let idx = (y * width + x) * 4;
            data[idx] = (((x + offset) * 255) / width.max(1)) as u8; // B
            data[idx + 1] = (((y + offset) * 255) / height.max(1)) as u8; // G
            data[idx + 2] = 128; // R
            data[idx + 3] = 255; // A
        }
    }
    data
}

/// Generate a frame with a small changed region (simulates cursor/typing)
fn generate_frame_with_damage(
    base: &[u8],
    width: usize,
    height: usize,
    damage_x: usize,
    damage_y: usize,
    damage_size: usize,
) -> Vec<u8> {
    let mut data = base.to_vec();

    // Modify a small region
    for y in damage_y..(damage_y + damage_size).min(height) {
        for x in damage_x..(damage_x + damage_size).min(width) {
            let idx = (y * width + x) * 4;
            if idx + 3 < data.len() {
                data[idx] = 255; // B
                data[idx + 1] = 255; // G
                data[idx + 2] = 255; // R
            }
        }
    }

    data
}

/// Benchmark damage detection with identical frames (best case - no damage)
fn bench_detect_no_damage(c: &mut Criterion) {
    let mut group = c.benchmark_group("damage_detect_no_damage");

    let resolutions = [
        (640, 480, "480p"),
        (1280, 720, "720p"),
        (1920, 1080, "1080p"),
        (3840, 2160, "4K"),
    ];

    for (width, height, name) in resolutions {
        let frame = generate_bgra_frame(width, height, 0);
        let pixels = (width * height) as u64;

        group.throughput(Throughput::Elements(pixels));

        group.bench_with_input(BenchmarkId::new("identical", name), &frame, |b, data| {
            let mut detector = DamageDetector::new(DamageConfig::default());
            // Prime with first frame
            let _ = detector.detect(data, width as u32, height as u32);

            b.iter(|| black_box(detector.detect(black_box(data), width as u32, height as u32)))
        });
    }

    group.finish();
}

/// Benchmark damage detection with full frame changes (worst case)
fn bench_detect_full_damage(c: &mut Criterion) {
    let mut group = c.benchmark_group("damage_detect_full_damage");

    let resolutions = [
        (640, 480, "480p"),
        (1280, 720, "720p"),
        (1920, 1080, "1080p"),
    ];

    for (width, height, name) in resolutions {
        let pixels = (width * height) as u64;

        group.throughput(Throughput::Elements(pixels));

        group.bench_function(BenchmarkId::new("full_change", name), |b| {
            let mut detector = DamageDetector::new(DamageConfig::default());
            let mut offset = 0usize;

            b.iter(|| {
                let frame = generate_bgra_frame(width, height, offset);
                offset += 100; // Each frame is completely different
                black_box(detector.detect(black_box(&frame), width as u32, height as u32))
            })
        });
    }

    group.finish();
}

/// Benchmark damage detection with partial changes (typical case - typing/cursor)
fn bench_detect_partial_damage(c: &mut Criterion) {
    let mut group = c.benchmark_group("damage_detect_partial_damage");

    let resolutions = [
        (640, 480, "480p"),
        (1280, 720, "720p"),
        (1920, 1080, "1080p"),
    ];

    for (width, height, name) in resolutions {
        let base_frame = generate_bgra_frame(width, height, 0);
        let pixels = (width * height) as u64;

        group.throughput(Throughput::Elements(pixels));

        // Small damage (cursor-like, 32x32)
        group.bench_function(BenchmarkId::new("small_32x32", name), |b| {
            let mut detector = DamageDetector::new(DamageConfig::default());
            let _ = detector.detect(&base_frame, width as u32, height as u32);
            let damaged = generate_frame_with_damage(&base_frame, width, height, 100, 100, 32);

            b.iter(|| {
                // Alternate between base and damaged to reset detector state
                let _ = detector.detect(&base_frame, width as u32, height as u32);
                black_box(detector.detect(black_box(&damaged), width as u32, height as u32))
            })
        });

        // Medium damage (small window, 256x256)
        group.bench_function(BenchmarkId::new("medium_256x256", name), |b| {
            let mut detector = DamageDetector::new(DamageConfig::default());
            let _ = detector.detect(&base_frame, width as u32, height as u32);
            let damaged = generate_frame_with_damage(&base_frame, width, height, 100, 100, 256);

            b.iter(|| {
                let _ = detector.detect(&base_frame, width as u32, height as u32);
                black_box(detector.detect(black_box(&damaged), width as u32, height as u32))
            })
        });
    }

    group.finish();
}

/// Benchmark region merging with various numbers of dirty tiles
fn bench_region_merging(c: &mut Criterion) {
    let mut group = c.benchmark_group("damage_region_merging");

    // Simulate many small changes that need merging
    let resolutions = [(1920, 1080, "1080p")];

    for (width, height, name) in resolutions {
        let base_frame = generate_bgra_frame(width, height, 0);

        // Create frame with scattered changes (simulates multi-window activity)
        let mut scattered_frame = base_frame.clone();
        for i in 0..10 {
            let x = (i * 150) % width;
            let y = (i * 100) % height;
            for dy in 0..64.min(height - y) {
                for dx in 0..64.min(width - x) {
                    let idx = ((y + dy) * width + (x + dx)) * 4;
                    if idx + 3 < scattered_frame.len() {
                        scattered_frame[idx] = ((i * 25) % 256) as u8;
                        scattered_frame[idx + 1] = ((i * 50) % 256) as u8;
                        scattered_frame[idx + 2] = ((i * 75) % 256) as u8;
                    }
                }
            }
        }

        group.bench_function(BenchmarkId::new("scattered_10_regions", name), |b| {
            let mut detector = DamageDetector::new(DamageConfig::default());
            let _ = detector.detect(&base_frame, width as u32, height as u32);

            b.iter(|| {
                let _ = detector.detect(&base_frame, width as u32, height as u32);
                black_box(detector.detect(black_box(&scattered_frame), width as u32, height as u32))
            })
        });
    }

    group.finish();
}

/// Benchmark with different tile sizes
fn bench_tile_sizes(c: &mut Criterion) {
    let mut group = c.benchmark_group("damage_tile_size_impact");

    let (width, height) = (1920, 1080);
    let frame1 = generate_bgra_frame(width, height, 0);
    let frame2 = generate_bgra_frame(width, height, 50);

    let tile_sizes = [32, 64, 128];

    for tile_size in tile_sizes {
        let config = DamageConfig {
            tile_size,
            ..Default::default()
        };

        group.bench_function(
            BenchmarkId::new("1080p", format!("{}px_tiles", tile_size)),
            |b| {
                let mut detector = DamageDetector::new(config.clone());
                let _ = detector.detect(&frame1, width as u32, height as u32);

                b.iter(|| {
                    let _ = detector.detect(&frame1, width as u32, height as u32);
                    black_box(detector.detect(black_box(&frame2), width as u32, height as u32))
                })
            },
        );
    }

    group.finish();
}

/// Benchmark DamageRegion operations
fn bench_damage_region_ops(c: &mut Criterion) {
    let mut group = c.benchmark_group("damage_region_operations");

    // Benchmark overlaps check
    group.bench_function("overlaps", |b| {
        let r1 = DamageRegion::new(0, 0, 100, 100);
        let r2 = DamageRegion::new(50, 50, 100, 100);

        b.iter(|| black_box(r1.overlaps(black_box(&r2))))
    });

    // Benchmark union
    group.bench_function("union", |b| {
        let r1 = DamageRegion::new(0, 0, 100, 100);
        let r2 = DamageRegion::new(50, 50, 100, 100);

        b.iter(|| black_box(r1.union(black_box(&r2))))
    });

    // Benchmark is_adjacent
    group.bench_function("is_adjacent", |b| {
        let r1 = DamageRegion::new(0, 0, 64, 64);
        let r2 = DamageRegion::new(80, 0, 64, 64);

        b.iter(|| black_box(r1.is_adjacent(black_box(&r2), 32)))
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_detect_no_damage,
    bench_detect_full_damage,
    bench_detect_partial_damage,
    bench_region_merging,
    bench_tile_sizes,
    bench_damage_region_ops
);
criterion_main!(benches);
