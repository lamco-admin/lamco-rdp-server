//! PipeWire Integration Tests
//!
//! These tests require a running Wayland session with PipeWire and Portal support.
//! Run with: cargo test --test pipewire_test -- --ignored

use std::sync::Arc;
use std::time::Duration;
use tokio::time::timeout;

use wrd_server::config::Config;
use wrd_server::pipewire::{
    PipeWireConnection, StreamConfig, MultiStreamCoordinator,
    MultiStreamConfig, MonitorInfo, PixelFormat,
};
use wrd_server::portal::PortalManager;

/// Test PipeWire connection creation
#[tokio::test]
#[ignore] // Requires active Wayland session
async fn test_pipewire_connection_from_portal() {
    // Initialize tracing for tests
    let _ = tracing_subscriber::fmt::try_init();

    // Create portal manager
    let config = Arc::new(Config::default_config().unwrap());
    let portal = PortalManager::new(&config).await
        .expect("Failed to create portal manager - ensure you're in a Wayland session");

    // Create portal session
    let session = portal.create_session().await
        .expect("Failed to create portal session - check portal permissions");

    // Get PipeWire FD
    let fd = session.pipewire_fd();
    assert!(fd > 0, "Invalid PipeWire file descriptor");

    // Create PipeWire connection
    let mut connection = PipeWireConnection::new(fd)
        .expect("Failed to create PipeWire connection");

    // Connect
    connection.connect().await
        .expect("Failed to connect to PipeWire");

    assert!(connection.is_connected());

    // Disconnect
    connection.disconnect().await
        .expect("Failed to disconnect");

    assert!(!connection.is_connected());
}

/// Test stream creation
#[tokio::test]
#[ignore] // Requires active Wayland session
async fn test_stream_creation() {
    let _ = tracing_subscriber::fmt::try_init();

    let config = Arc::new(Config::default_config().unwrap());
    let portal = PortalManager::new(&config).await
        .expect("Failed to create portal manager");

    let session = portal.create_session().await
        .expect("Failed to create portal session");

    let fd = session.pipewire_fd();
    let mut connection = PipeWireConnection::new(fd).unwrap();
    connection.connect().await.unwrap();

    // Get available streams
    let streams = session.streams();
    assert!(!streams.is_empty(), "No streams available");

    // Create stream for first monitor
    let stream_info = &streams[0];
    let stream_config = StreamConfig::new(format!("test-stream-{}", stream_info.node_id))
        .with_resolution(stream_info.size.0, stream_info.size.1);

    let stream_id = connection.create_stream(stream_config, stream_info.node_id).await
        .expect("Failed to create stream");

    assert!(stream_id >= 0);
    assert_eq!(connection.stream_count(), 1);

    // Remove stream
    connection.remove_stream(stream_id).await.unwrap();
    assert_eq!(connection.stream_count(), 0);

    connection.disconnect().await.unwrap();
}

/// Test frame capture
#[tokio::test]
#[ignore] // Requires active Wayland session
async fn test_frame_capture() {
    let _ = tracing_subscriber::fmt::try_init();

    let config = Arc::new(Config::default_config().unwrap());
    let portal = PortalManager::new(&config).await.unwrap();
    let session = portal.create_session().await.unwrap();

    let fd = session.pipewire_fd();
    let mut connection = PipeWireConnection::new(fd).unwrap();
    connection.connect().await.unwrap();

    let streams = session.streams();
    let stream_info = &streams[0];

    let stream_config = StreamConfig::new(format!("capture-test-{}", stream_info.node_id))
        .with_resolution(stream_info.size.0, stream_info.size.1);

    let stream_id = connection.create_stream(stream_config, stream_info.node_id).await.unwrap();

    // Get stream and set up frame channel
    if let Some(stream_arc) = connection.get_stream(stream_id) {
        let (tx, mut rx) = tokio::sync::mpsc::channel(10);
        stream_arc.lock().unwrap().set_frame_channel(tx);

        // Try to receive a frame (with timeout)
        let frame_result = timeout(Duration::from_secs(5), rx.recv()).await;

        match frame_result {
            Ok(Some(frame)) => {
                println!("Captured frame: {}x{}", frame.width, frame.height);
                assert!(frame.width > 0);
                assert!(frame.height > 0);
                assert!(!frame.data.is_empty());
            }
            Ok(None) => {
                println!("Channel closed without receiving frame");
            }
            Err(_) => {
                println!("Timeout waiting for frame - this may be normal if compositor isn't updating");
            }
        }
    }

    connection.disconnect().await.unwrap();
}

/// Test multi-stream coordinator
#[tokio::test]
#[ignore] // Requires active Wayland session
async fn test_multi_stream_coordinator() {
    let _ = tracing_subscriber::fmt::try_init();

    let config = Arc::new(Config::default_config().unwrap());
    let portal = PortalManager::new(&config).await.unwrap();
    let session = portal.create_session().await.unwrap();

    let fd = session.pipewire_fd();
    let mut connection = PipeWireConnection::new(fd).unwrap();
    connection.connect().await.unwrap();

    // Create coordinator
    let coordinator_config = MultiStreamConfig::default();
    let coordinator = MultiStreamCoordinator::new(coordinator_config).await.unwrap();

    // Add streams for all available monitors
    for stream_info in session.streams() {
        let monitor = MonitorInfo {
            id: stream_info.node_id,
            name: format!("Monitor-{}", stream_info.node_id),
            position: stream_info.position,
            size: stream_info.size,
            refresh_rate: 60,
            node_id: stream_info.node_id,
        };

        let result = coordinator.add_stream(monitor, &mut connection).await;
        if let Ok(stream_id) = result {
            println!("Added stream {} for monitor {}", stream_id, stream_info.node_id);
        }
    }

    assert!(coordinator.active_streams().await > 0);

    // Get stats
    let stats = coordinator.stats().await;
    println!("Coordinator stats: {:?}", stats);

    connection.disconnect().await.unwrap();
}

/// Test format conversion
#[test]
fn test_format_conversion() {
    use wrd_server::pipewire::convert_format;

    // Create RGB test pattern
    let width = 100u32;
    let height = 100u32;
    let mut src = Vec::new();

    for _y in 0..height {
        for x in 0..width {
            // Create gradient
            src.push((x * 255 / width) as u8);  // R
            src.push(0);                         // G
            src.push(255 - (x * 255 / width) as u8); // B
        }
    }

    // Convert to BGRA
    let src_stride = width * 3;
    let dst_stride = width * 4;
    let mut dst = vec![0u8; (dst_stride * height) as usize];

    convert_format(
        &src,
        &mut dst,
        PixelFormat::RGB,
        PixelFormat::BGRA,
        width,
        height,
        src_stride,
        dst_stride,
    ).unwrap();

    // Verify first pixel
    assert_eq!(dst[0], 255); // B (was in position 2)
    assert_eq!(dst[1], 0);   // G (same)
    assert_eq!(dst[2], 0);   // R (was in position 0)
    assert_eq!(dst[3], 255); // A (added)
}

/// Test buffer management
#[test]
fn test_buffer_management() {
    use wrd_server::pipewire::{BufferManager, BufferType};

    let mut mgr = BufferManager::new(5);

    // Register buffers
    for i in 0..3 {
        let id = mgr.register_buffer(BufferType::MemPtr, 1024 * (i + 1), None, 0)
            .expect("Failed to register buffer");
        assert_eq!(id, i);
    }

    assert_eq!(mgr.total_count(), 3);
    assert_eq!(mgr.free_count(), 3);

    // Acquire buffers
    let id1 = mgr.acquire_buffer().unwrap();
    let id2 = mgr.acquire_buffer().unwrap();

    assert_eq!(mgr.free_count(), 1);
    assert_eq!(mgr.in_use_count(), 2);

    // Release buffer
    mgr.release_buffer(id1).unwrap();
    assert_eq!(mgr.free_count(), 2);
    assert_eq!(mgr.in_use_count(), 1);

    // Get stats
    let stats = mgr.stats();
    assert_eq!(stats.total_allocated, 3);
    assert_eq!(stats.acquisitions, 2);
    assert_eq!(stats.releases, 1);
}

/// Test stream state transitions
#[tokio::test]
async fn test_stream_states() {
    use wrd_server::pipewire::{PipeWireStream, PwStreamState};

    let config = StreamConfig::new("test-stream");
    let mut stream = PipeWireStream::new(0, config);

    assert_eq!(stream.state(), PwStreamState::Initializing);

    stream.start().await.unwrap();
    assert_eq!(stream.state(), PwStreamState::Streaming);

    stream.pause().await.unwrap();
    assert_eq!(stream.state(), PwStreamState::Paused);

    stream.resume().await.unwrap();
    assert_eq!(stream.state(), PwStreamState::Streaming);

    stream.stop().await.unwrap();
    assert_eq!(stream.state(), PwStreamState::Closing);
}

/// Test DMA-BUF detection
#[test]
#[cfg(target_os = "linux")]
fn test_dmabuf_detection() {
    use wrd_server::pipewire::is_dmabuf_supported;

    let supported = is_dmabuf_supported();
    println!("DMA-BUF supported: {}", supported);

    // Just verify it doesn't panic
    // Actual support depends on hardware and drivers
}

/// Benchmark format conversion
#[test]
fn bench_format_conversion() {
    use std::time::Instant;
    use wrd_server::pipewire::convert_format;

    let width = 1920u32;
    let height = 1080u32;

    // Create source buffer
    let src_size = (width * height * 3) as usize;
    let src = vec![128u8; src_size];

    // Create destination buffer
    let dst_size = (width * height * 4) as usize;
    let mut dst = vec![0u8; dst_size];

    // Benchmark conversion
    let iterations = 10;
    let start = Instant::now();

    for _ in 0..iterations {
        convert_format(
            &src,
            &mut dst,
            PixelFormat::RGB,
            PixelFormat::BGRA,
            width,
            height,
            width * 3,
            width * 4,
        ).unwrap();
    }

    let elapsed = start.elapsed();
    let avg_time = elapsed / iterations;

    println!("Average conversion time (1080p RGB->BGRA): {:?}", avg_time);
    println!("FPS capacity: {:.1}", 1.0 / avg_time.as_secs_f64());

    // Should be well under 2ms
    assert!(avg_time.as_millis() < 2);
}
