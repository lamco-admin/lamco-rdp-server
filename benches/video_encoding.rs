//! Video Encoding Benchmarks
//!
//! Measures AVC444 H.264 encoding performance at various resolutions.
//! Requires the `h264` feature to be enabled.

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
#[cfg(feature = "h264")]
use lamco_rdp_server::egfx::{Avc444Encoder, EncoderConfig};

/// Generate test BGRA data with varying content
fn generate_bgra_frame(width: usize, height: usize, frame_num: u32) -> Vec<u8> {
    let mut data = vec![0u8; width * height * 4];
    let offset = (frame_num * 10) as usize;

    for y in 0..height {
        for x in 0..width {
            let idx = (y * width + x) * 4;
            // Create moving gradient pattern for realistic compression
            data[idx] = (((x + offset) * 255) / width) as u8; // B
            data[idx + 1] = (((y + offset) * 255) / height) as u8; // G
            data[idx + 2] = ((128 + (frame_num as usize * 5)) % 256) as u8; // R
            data[idx + 3] = 255; // A
        }
    }
    data
}

/// Benchmark AVC444 encoder creation (initialization cost)
#[cfg(feature = "h264")]
fn bench_encoder_creation(c: &mut Criterion) {
    let mut group = c.benchmark_group("avc444_encoder_init");

    let resolutions = [
        (640, 480, "480p"),
        (1280, 720, "720p"),
        (1920, 1080, "1080p"),
    ];

    for (width, height, name) in resolutions {
        group.bench_function(BenchmarkId::new("create", name), |b| {
            b.iter(|| {
                let config = EncoderConfig {
                    width: Some(black_box(width)),
                    height: Some(black_box(height)),
                    bitrate_kbps: 5000,
                    ..Default::default()
                };
                black_box(Avc444Encoder::new(config).unwrap())
            });
        });
    }

    group.finish();
}

/// Benchmark single frame encoding (keyframe)
#[cfg(feature = "h264")]
fn bench_encode_single_frame(c: &mut Criterion) {
    let mut group = c.benchmark_group("avc444_encode_frame");
    group.sample_size(30); // Reduce samples for expensive operations

    let resolutions = [
        (640, 480, "480p"),
        (1280, 720, "720p"),
        (1920, 1080, "1080p"),
    ];

    for (width, height, name) in resolutions {
        let bgra_data = generate_bgra_frame(width as usize, height as usize, 0);
        let pixels = (width * height) as u64;

        group.throughput(Throughput::Elements(pixels));

        group.bench_with_input(BenchmarkId::new("keyframe", name), &bgra_data, |b, data| {
            let config = EncoderConfig {
                width: Some(width),
                height: Some(height),
                bitrate_kbps: 5000,
                ..Default::default()
            };
            let mut encoder = Avc444Encoder::new(config).unwrap();

            b.iter(|| {
                // Force keyframe each iteration
                encoder.force_keyframe();
                black_box(
                    encoder
                        .encode_bgra(black_box(data), width as u32, height as u32, 0)
                        .unwrap(),
                )
            });
        });
    }

    group.finish();
}

/// Benchmark P-frame encoding (after initial keyframe)
#[cfg(feature = "h264")]
fn bench_encode_p_frames(c: &mut Criterion) {
    let mut group = c.benchmark_group("avc444_encode_p_frame");
    group.sample_size(50);

    let resolutions = [
        (640, 480, "480p"),
        (1280, 720, "720p"),
        (1920, 1080, "1080p"),
    ];

    for (width, height, name) in resolutions {
        let pixels = (width * height) as u64;

        group.throughput(Throughput::Elements(pixels));

        group.bench_function(BenchmarkId::new("p_frame", name), |b| {
            let config = EncoderConfig {
                width: Some(width),
                height: Some(height),
                bitrate_kbps: 5000,
                ..Default::default()
            };
            let mut encoder = Avc444Encoder::new(config).unwrap();

            // Encode initial keyframe
            let keyframe_data = generate_bgra_frame(width as usize, height as usize, 0);
            let _ = encoder
                .encode_bgra(&keyframe_data, width as u32, height as u32, 0)
                .unwrap();

            let mut frame_num = 1u32;

            b.iter(|| {
                let data = generate_bgra_frame(width as usize, height as usize, frame_num);
                frame_num += 1;
                black_box(
                    encoder
                        .encode_bgra(
                            black_box(&data),
                            width as u32,
                            height as u32,
                            frame_num as u64 * 33,
                        )
                        .unwrap(),
                )
            });
        });
    }

    group.finish();
}

/// Benchmark sustained encoding (30 frame sequence)
#[cfg(feature = "h264")]
fn bench_encode_sequence(c: &mut Criterion) {
    let mut group = c.benchmark_group("avc444_encode_sequence");
    group.sample_size(10); // Expensive operation

    let resolutions = [(640, 480, "480p"), (1280, 720, "720p")];

    for (width, height, name) in resolutions {
        let frames: Vec<Vec<u8>> = (0..30)
            .map(|i| generate_bgra_frame(width as usize, height as usize, i))
            .collect();

        let total_pixels = (width * height * 30) as u64;

        group.throughput(Throughput::Elements(total_pixels));

        group.bench_with_input(
            BenchmarkId::new("30_frames", name),
            &frames,
            |b, frame_data| {
                b.iter(|| {
                    let config = EncoderConfig {
                        width: Some(width),
                        height: Some(height),
                        bitrate_kbps: 5000,
                        ..Default::default()
                    };
                    let mut encoder = Avc444Encoder::new(config).unwrap();

                    for (i, data) in frame_data.iter().enumerate() {
                        let _ = black_box(
                            encoder
                                .encode_bgra(data, width as u32, height as u32, (i * 33) as u64)
                                .unwrap(),
                        );
                    }
                });
            },
        );
    }

    group.finish();
}

/// Benchmark encoder with different bitrate configurations
#[cfg(feature = "h264")]
fn bench_bitrate_impact(c: &mut Criterion) {
    let mut group = c.benchmark_group("avc444_bitrate_impact");
    group.sample_size(20);

    let bitrates = [1000, 3000, 5000, 8000];
    let (width, height) = (1280, 720);
    let bgra_data = generate_bgra_frame(width as usize, height as usize, 0);

    for bitrate in bitrates {
        group.bench_function(BenchmarkId::new("720p", format!("{bitrate}kbps")), |b| {
            let config = EncoderConfig {
                width: Some(width),
                height: Some(height),
                bitrate_kbps: bitrate,
                ..Default::default()
            };
            let mut encoder = Avc444Encoder::new(config).unwrap();

            b.iter(|| {
                encoder.force_keyframe();
                black_box(
                    encoder
                        .encode_bgra(&bgra_data, width as u32, height as u32, 0)
                        .unwrap(),
                )
            });
        });
    }

    group.finish();
}

// Placeholder for when h264 feature is not enabled
#[cfg(not(feature = "h264"))]
fn bench_encoder_creation(_c: &mut Criterion) {}

#[cfg(not(feature = "h264"))]
fn bench_encode_single_frame(_c: &mut Criterion) {}

#[cfg(not(feature = "h264"))]
fn bench_encode_p_frames(_c: &mut Criterion) {}

#[cfg(not(feature = "h264"))]
fn bench_encode_sequence(_c: &mut Criterion) {}

#[cfg(not(feature = "h264"))]
fn bench_bitrate_impact(_c: &mut Criterion) {}

criterion_group!(
    benches,
    bench_encoder_creation,
    bench_encode_single_frame,
    bench_encode_p_frames,
    bench_encode_sequence,
    bench_bitrate_impact
);
criterion_main!(benches);
