//! Integration test for portal functionality
//!
//! NOTE: This test requires a running Wayland session with xdg-desktop-portal
//! Run manually with: cargo test --test portal_test -- --ignored --nocapture

use wrd_server::portal::PortalManager;
use wrd_server::config::Config;
use std::sync::Arc;

#[tokio::test]
#[ignore] // Only run manually in Wayland session
async fn test_create_portal_session() {
    // Initialize logging for test
    let _ = tracing_subscriber::fmt()
        .with_env_filter("debug")
        .try_init();

    println!("Creating portal session...");
    println!("NOTE: This will show a permission dialog on your desktop.");
    println!("Please APPROVE the request to continue.");

    // Create config
    let config = Arc::new(Config::default_config().unwrap());

    // Create portal manager
    let portal_manager = PortalManager::new(&config)
        .await
        .expect("Failed to create portal manager");

    println!("Portal manager created successfully");

    // Create session (this will show permission dialog)
    let session = portal_manager.create_session()
        .await
        .expect("Failed to create portal session");

    println!("Portal session created!");
    println!("  Session ID: {}", session.session_id());
    println!("  PipeWire FD: {}", session.pipewire_fd());
    println!("  Streams: {}", session.streams().len());

    // Verify FD is valid
    assert!(session.pipewire_fd() > 0, "PipeWire FD should be positive");

    // Verify we have at least one stream
    assert!(!session.streams().is_empty(), "Should have at least one stream");

    // Print stream details
    for (i, stream) in session.streams().iter().enumerate() {
        println!("  Stream {}: node_id={}, size={}x{}, pos=({},{})",
            i, stream.node_id, stream.size.0, stream.size.1,
            stream.position.0, stream.position.1);
    }

    // Test input injection
    println!("\nTesting input injection...");
    let rd_manager = portal_manager.remote_desktop();

    // Move mouse (relative)
    rd_manager.notify_pointer_motion(10.0, 10.0)
        .await
        .expect("Failed to inject pointer motion");
    println!("  ✓ Pointer motion injected");

    // Press and release a key (keycode 28 = Enter on most systems)
    rd_manager.notify_keyboard_keycode(28, true)
        .await
        .expect("Failed to inject key press");
    rd_manager.notify_keyboard_keycode(28, false)
        .await
        .expect("Failed to inject key release");
    println!("  ✓ Keyboard input injected");

    println!("\n✅ All portal tests passed!");

    // Cleanup
    session.close().await.expect("Failed to close session");
    println!("Session closed");
}

#[tokio::test]
#[ignore]
async fn test_portal_input_injection() {
    let _ = tracing_subscriber::fmt().with_env_filter("debug").try_init();

    println!("Testing portal input injection...");

    let config = Arc::new(Config::default_config().unwrap());
    let portal_manager = PortalManager::new(&config).await.unwrap();
    let session = portal_manager.create_session().await.unwrap();

    // Test various input methods
    let rd = portal_manager.remote_desktop();

    // Mouse movements
    rd.notify_pointer_motion(5.0, 5.0).await.unwrap();
    rd.notify_pointer_motion(-5.0, -5.0).await.unwrap();

    // Mouse button
    rd.notify_pointer_button(272, true).await.unwrap(); // Left click press
    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
    rd.notify_pointer_button(272, false).await.unwrap(); // Left click release

    // Keyboard
    rd.notify_keyboard_keycode(57, true).await.unwrap(); // Space press
    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
    rd.notify_keyboard_keycode(57, false).await.unwrap(); // Space release

    println!("✅ Input injection test passed");

    session.close().await.unwrap();
}
