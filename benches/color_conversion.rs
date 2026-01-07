//! Color Conversion Benchmarks
//!
//! Measures performance of BGRA→YUV444 conversion at various resolutions.
//! Tests both scalar and SIMD (AVX2/NEON) code paths.

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use lamco_rdp_server::egfx::{bgra_to_yuv444, pack_dual_views, ColorMatrix};

/// Generate test BGRA data with a gradient pattern
fn generate_bgra_data(width: usize, height: usize) -> Vec<u8> {
    let mut data = vec![0u8; width * height * 4];
    for y in 0..height {
        for x in 0..width {
            let idx = (y * width + x) * 4;
            data[idx] = ((x * 255) / width) as u8; // B
            data[idx + 1] = ((y * 255) / height) as u8; // G
            data[idx + 2] = 128; // R
            data[idx + 3] = 255; // A
        }
    }
    data
}

/// Benchmark BGRA→YUV444 conversion at various resolutions
fn bench_bgra_to_yuv444(c: &mut Criterion) {
    let mut group = c.benchmark_group("bgra_to_yuv444");

    // Test resolutions: SD, 720p, 1080p, 4K
    let resolutions = [
        (640, 480, "480p"),
        (1280, 720, "720p"),
        (1920, 1080, "1080p"),
        (3840, 2160, "4K"),
    ];

    for (width, height, name) in resolutions {
        let bgra_data = generate_bgra_data(width, height);
        let pixels = (width * height) as u64;

        group.throughput(Throughput::Elements(pixels));

        // Benchmark with BT.709 (HD)
        group.bench_with_input(BenchmarkId::new("BT709", name), &bgra_data, |b, data| {
            b.iter(|| {
                black_box(bgra_to_yuv444(
                    black_box(data),
                    black_box(width),
                    black_box(height),
                    ColorMatrix::BT709,
                ))
            })
        });

        // Benchmark with BT.601 (SD)
        group.bench_with_input(BenchmarkId::new("BT601", name), &bgra_data, |b, data| {
            b.iter(|| {
                black_box(bgra_to_yuv444(
                    black_box(data),
                    black_box(width),
                    black_box(height),
                    ColorMatrix::BT601,
                ))
            })
        });
    }

    group.finish();
}

/// Benchmark chroma subsampling (YUV444→YUV420)
fn bench_chroma_subsample(c: &mut Criterion) {
    let mut group = c.benchmark_group("chroma_subsample");

    let resolutions = [
        (640, 480, "480p"),
        (1280, 720, "720p"),
        (1920, 1080, "1080p"),
    ];

    for (width, height, name) in resolutions {
        let bgra_data = generate_bgra_data(width, height);
        let yuv444 = bgra_to_yuv444(&bgra_data, width, height, ColorMatrix::BT709);
        let pixels = (width * height) as u64;

        group.throughput(Throughput::Elements(pixels));

        group.bench_with_input(
            BenchmarkId::new("pack_dual_views", name),
            &yuv444,
            |b, frame| b.iter(|| black_box(pack_dual_views(black_box(frame)))),
        );
    }

    group.finish();
}

/// Benchmark full color pipeline (BGRA→YUV444→dual YUV420)
fn bench_full_color_pipeline(c: &mut Criterion) {
    let mut group = c.benchmark_group("full_color_pipeline");

    let resolutions = [
        (640, 480, "480p"),
        (1280, 720, "720p"),
        (1920, 1080, "1080p"),
    ];

    for (width, height, name) in resolutions {
        let bgra_data = generate_bgra_data(width, height);
        let pixels = (width * height) as u64;

        group.throughput(Throughput::Elements(pixels));

        group.bench_with_input(
            BenchmarkId::new("bgra_to_dual_yuv420", name),
            &bgra_data,
            |b, data| {
                b.iter(|| {
                    let yuv444 = bgra_to_yuv444(
                        black_box(data),
                        black_box(width),
                        black_box(height),
                        ColorMatrix::BT709,
                    );
                    black_box(pack_dual_views(&yuv444))
                })
            },
        );
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_bgra_to_yuv444,
    bench_chroma_subsample,
    bench_full_color_pipeline
);
criterion_main!(benches);
