//! Multi-Monitor Tracking
//!
//! **Execution Path:** Portal ScreenCast streams
//! **Status:** Active (v1.0.0+)
//! **Platform:** Universal
//! **Role:** Monitor discovery, layout calculation, and state tracking
//!
//! Manages monitor discovery, tracking, and lifecycle with Portal integration.

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info};

use crate::multimon::layout::{Layout, LayoutCalculator, LayoutStrategy};
use crate::multimon::Result;
use crate::portal::StreamInfo;

#[derive(Debug, Clone)]
pub struct MonitorInfo {
    pub id: u32,
    pub name: String,
    pub position: (i32, i32),
    pub size: (u32, u32),
    pub refresh_rate: u32,
    pub scale_factor: f64,
    pub is_primary: bool,
}

impl MonitorInfo {
    pub fn from_stream_info(stream: &StreamInfo, is_primary: bool) -> Self {
        Self {
            id: stream.node_id,
            name: format!("Monitor {}", stream.node_id),
            position: stream.position,
            size: stream.size,
            refresh_rate: 60, // Default, would be extracted from stream metadata
            scale_factor: 1.0,
            is_primary,
        }
    }
}

#[derive(Debug, Clone)]
pub enum MonitorEvent {
    Added(MonitorInfo),
    Removed(u32),
    Changed(MonitorInfo),
    LayoutChanged(Layout),
}

#[derive(Debug, Clone)]
pub struct MultiMonitorConfig {
    pub max_monitors: usize,
    pub layout_strategy: LayoutStrategy,
    pub enable_dynamic_reconfiguration: bool,
    pub enable_hotplug: bool,
}

impl Default for MultiMonitorConfig {
    fn default() -> Self {
        Self {
            max_monitors: 16,
            layout_strategy: LayoutStrategy::PreservePositions,
            enable_dynamic_reconfiguration: true,
            enable_hotplug: true,
        }
    }
}

pub struct Monitors {
    config: MultiMonitorConfig,
    monitors: Arc<RwLock<HashMap<u32, MonitorInfo>>>,
    layout_calculator: LayoutCalculator,
    current_layout: Arc<RwLock<Option<Layout>>>,
}

impl Monitors {
    pub fn new(config: MultiMonitorConfig) -> Self {
        let layout_calculator = LayoutCalculator::new(config.layout_strategy);

        Self {
            config,
            monitors: Arc::new(RwLock::new(HashMap::new())),
            layout_calculator,
            current_layout: Arc::new(RwLock::new(None)),
        }
    }

    pub async fn initialize_from_streams(&self, streams: &[StreamInfo]) -> Result<()> {
        info!("Initializing monitors from {} streams", streams.len());

        let mut monitors_map = HashMap::new();

        for (idx, stream) in streams.iter().enumerate() {
            let monitor = MonitorInfo::from_stream_info(stream, idx == 0);
            debug!(
                "Monitor {}: {}x{} at ({}, {})",
                monitor.id, monitor.size.0, monitor.size.1, monitor.position.0, monitor.position.1
            );
            monitors_map.insert(monitor.id, monitor);
        }

        *self.monitors.write().await = monitors_map;
        self.recalculate_layout(streams).await?;

        info!("Multi-monitor initialization complete");
        Ok(())
    }

    pub async fn recalculate_layout(&self, streams: &[StreamInfo]) -> Result<()> {
        let virtual_desktop = self.layout_calculator.calculate_layout(streams)?;

        let layout = Layout::from_virtual_desktop(virtual_desktop);

        debug!(
            "Layout recalculated: {}x{}",
            layout.virtual_desktop.width, layout.virtual_desktop.height
        );

        *self.current_layout.write().await = Some(layout);

        Ok(())
    }

    pub async fn get_layout(&self) -> Option<Layout> {
        self.current_layout.read().await.clone()
    }

    pub async fn monitor_count(&self) -> usize {
        self.monitors.read().await.len()
    }

    pub async fn get_monitor(&self, id: u32) -> Option<MonitorInfo> {
        self.monitors.read().await.get(&id).cloned()
    }

    pub async fn get_all_monitors(&self) -> Vec<MonitorInfo> {
        self.monitors.read().await.values().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::portal::SourceType;

    fn mock_stream(node_id: u32, x: i32, y: i32, width: u32, height: u32) -> StreamInfo {
        StreamInfo {
            node_id,
            position: (x, y),
            size: (width, height),
            source_type: SourceType::Monitor,
        }
    }

    #[test]
    fn test_config_default() {
        let config = MultiMonitorConfig::default();
        assert_eq!(config.max_monitors, 16);
        assert!(matches!(
            config.layout_strategy,
            LayoutStrategy::PreservePositions
        ));
        assert!(config.enable_dynamic_reconfiguration);
        assert!(config.enable_hotplug);
    }

    #[test]
    fn test_config_clone() {
        let config = MultiMonitorConfig {
            max_monitors: 8,
            layout_strategy: LayoutStrategy::Horizontal,
            enable_dynamic_reconfiguration: false,
            enable_hotplug: false,
        };
        let cloned = config.clone();
        assert_eq!(cloned.max_monitors, 8);
        assert!(matches!(cloned.layout_strategy, LayoutStrategy::Horizontal));
        assert!(!cloned.enable_dynamic_reconfiguration);
        assert!(!cloned.enable_hotplug);
    }

    #[test]
    fn test_config_debug() {
        let config = MultiMonitorConfig::default();
        let debug_str = format!("{:?}", config);
        assert!(debug_str.contains("MultiMonitorConfig"));
        assert!(debug_str.contains("max_monitors"));
    }

    #[tokio::test]
    async fn test_monitor_manager_creation() {
        let config = MultiMonitorConfig::default();
        let _manager = Monitors::new(config);
    }

    #[tokio::test]
    async fn test_monitor_from_stream_info() {
        let stream = StreamInfo {
            node_id: 42,
            position: (0, 0),
            size: (1920, 1080),
            source_type: SourceType::Monitor,
        };

        let monitor = MonitorInfo::from_stream_info(&stream, true);
        assert_eq!(monitor.id, 42);
        assert_eq!(monitor.size, (1920, 1080));
        assert!(monitor.is_primary);
    }

    #[test]
    fn test_monitor_info_non_primary() {
        let stream = mock_stream(99, 1920, 0, 1280, 720);
        let monitor = MonitorInfo::from_stream_info(&stream, false);
        assert_eq!(monitor.id, 99);
        assert_eq!(monitor.position, (1920, 0));
        assert_eq!(monitor.size, (1280, 720));
        assert!(!monitor.is_primary);
        assert_eq!(monitor.refresh_rate, 60);
        assert_eq!(monitor.scale_factor, 1.0);
    }

    #[test]
    fn test_monitor_info_name_generation() {
        let stream = mock_stream(123, 0, 0, 1920, 1080);
        let monitor = MonitorInfo::from_stream_info(&stream, true);
        assert_eq!(monitor.name, "Monitor 123");
    }

    #[test]
    fn test_monitor_info_clone() {
        let stream = mock_stream(1, 0, 0, 2560, 1440);
        let monitor = MonitorInfo::from_stream_info(&stream, true);
        let cloned = monitor.clone();
        assert_eq!(cloned.id, monitor.id);
        assert_eq!(cloned.name, monitor.name);
        assert_eq!(cloned.position, monitor.position);
        assert_eq!(cloned.size, monitor.size);
        assert_eq!(cloned.is_primary, monitor.is_primary);
    }

    #[test]
    fn test_monitor_info_debug() {
        let stream = mock_stream(1, 0, 0, 1920, 1080);
        let monitor = MonitorInfo::from_stream_info(&stream, true);
        let debug_str = format!("{:?}", monitor);
        assert!(debug_str.contains("MonitorInfo"));
        assert!(debug_str.contains("id: 1"));
    }

    #[test]
    fn test_monitor_event_added() {
        let stream = mock_stream(1, 0, 0, 1920, 1080);
        let monitor = MonitorInfo::from_stream_info(&stream, true);
        let event = MonitorEvent::Added(monitor.clone());
        if let MonitorEvent::Added(m) = event {
            assert_eq!(m.id, 1);
        } else {
            panic!("Expected MonitorEvent::Added");
        }
    }

    #[test]
    fn test_monitor_event_removed() {
        let event = MonitorEvent::Removed(42);
        if let MonitorEvent::Removed(id) = event {
            assert_eq!(id, 42);
        } else {
            panic!("Expected MonitorEvent::Removed");
        }
    }

    #[test]
    fn test_monitor_event_changed() {
        let stream = mock_stream(5, 100, 200, 2560, 1440);
        let monitor = MonitorInfo::from_stream_info(&stream, false);
        let event = MonitorEvent::Changed(monitor.clone());
        if let MonitorEvent::Changed(m) = event {
            assert_eq!(m.id, 5);
            assert_eq!(m.size, (2560, 1440));
        } else {
            panic!("Expected MonitorEvent::Changed");
        }
    }

    #[test]
    fn test_monitor_event_clone() {
        let event = MonitorEvent::Removed(99);
        let cloned = event.clone();
        assert!(matches!(cloned, MonitorEvent::Removed(99)));
    }

    #[test]
    fn test_monitor_event_debug() {
        let event = MonitorEvent::Removed(42);
        let debug_str = format!("{:?}", event);
        assert!(debug_str.contains("Removed"));
        assert!(debug_str.contains("42"));
    }

    #[tokio::test]
    async fn test_initialize_from_streams_single() {
        let config = MultiMonitorConfig::default();
        let manager = Monitors::new(config);

        let streams = vec![mock_stream(1, 0, 0, 1920, 1080)];
        manager.initialize_from_streams(&streams).await.unwrap();

        assert_eq!(manager.monitor_count().await, 1);

        let monitor = manager.get_monitor(1).await.unwrap();
        assert_eq!(monitor.id, 1);
        assert!(monitor.is_primary);
    }

    #[tokio::test]
    async fn test_initialize_from_streams_dual() {
        let config = MultiMonitorConfig::default();
        let manager = Monitors::new(config);

        let streams = vec![
            mock_stream(1, 0, 0, 1920, 1080),
            mock_stream(2, 1920, 0, 1920, 1080),
        ];
        manager.initialize_from_streams(&streams).await.unwrap();

        assert_eq!(manager.monitor_count().await, 2);

        let primary = manager.get_monitor(1).await.unwrap();
        assert!(primary.is_primary);

        let secondary = manager.get_monitor(2).await.unwrap();
        assert!(!secondary.is_primary);
    }

    #[tokio::test]
    async fn test_initialize_from_streams_multiple() {
        let config = MultiMonitorConfig::default();
        let manager = Monitors::new(config);

        let streams = vec![
            mock_stream(1, 0, 0, 1920, 1080),
            mock_stream(2, 1920, 0, 1920, 1080),
            mock_stream(3, 0, 1080, 1920, 1080),
            mock_stream(4, 1920, 1080, 1920, 1080),
        ];
        manager.initialize_from_streams(&streams).await.unwrap();

        assert_eq!(manager.monitor_count().await, 4);
    }

    #[tokio::test]
    async fn test_get_all_monitors() {
        let config = MultiMonitorConfig::default();
        let manager = Monitors::new(config);

        let streams = vec![
            mock_stream(10, 0, 0, 1920, 1080),
            mock_stream(20, 1920, 0, 2560, 1440),
        ];
        manager.initialize_from_streams(&streams).await.unwrap();

        let all_monitors = manager.get_all_monitors().await;
        assert_eq!(all_monitors.len(), 2);

        let ids: Vec<u32> = all_monitors.iter().map(|m| m.id).collect();
        assert!(ids.contains(&10));
        assert!(ids.contains(&20));
    }

    #[tokio::test]
    async fn test_get_monitor_not_found() {
        let config = MultiMonitorConfig::default();
        let manager = Monitors::new(config);

        let streams = vec![mock_stream(1, 0, 0, 1920, 1080)];
        manager.initialize_from_streams(&streams).await.unwrap();

        let result = manager.get_monitor(999).await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_get_layout() {
        let config = MultiMonitorConfig::default();
        let manager = Monitors::new(config);

        assert!(manager.get_layout().await.is_none());

        let streams = vec![
            mock_stream(1, 0, 0, 1920, 1080),
            mock_stream(2, 1920, 0, 1920, 1080),
        ];
        manager.initialize_from_streams(&streams).await.unwrap();

        let layout = manager.get_layout().await.unwrap();
        assert_eq!(layout.virtual_desktop.width, 3840);
        assert_eq!(layout.virtual_desktop.height, 1080);
    }

    #[tokio::test]
    async fn test_recalculate_layout() {
        let config = MultiMonitorConfig::default();
        let manager = Monitors::new(config);

        let streams = vec![mock_stream(1, 0, 0, 1920, 1080)];
        manager.initialize_from_streams(&streams).await.unwrap();

        let layout1 = manager.get_layout().await.unwrap();
        assert_eq!(layout1.virtual_desktop.width, 1920);

        let new_streams = vec![
            mock_stream(1, 0, 0, 1920, 1080),
            mock_stream(2, 1920, 0, 2560, 1440),
        ];
        manager.recalculate_layout(&new_streams).await.unwrap();

        let layout2 = manager.get_layout().await.unwrap();
        // PreservePositions strategy: 1920 + 2560 = 4480 width
        assert_eq!(layout2.virtual_desktop.width, 4480);
    }

    #[tokio::test]
    async fn test_manager_with_horizontal_strategy() {
        let config = MultiMonitorConfig {
            layout_strategy: LayoutStrategy::Horizontal,
            ..Default::default()
        };
        let manager = Monitors::new(config);

        let streams = vec![
            mock_stream(1, 0, 0, 1920, 1080),
            mock_stream(2, 0, 0, 1920, 1080),
        ];
        manager.initialize_from_streams(&streams).await.unwrap();

        let layout = manager.get_layout().await.unwrap();
        assert_eq!(layout.virtual_desktop.width, 3840);
        assert_eq!(layout.virtual_desktop.height, 1080);
    }

    #[tokio::test]
    async fn test_manager_with_vertical_strategy() {
        let config = MultiMonitorConfig {
            layout_strategy: LayoutStrategy::Vertical,
            ..Default::default()
        };
        let manager = Monitors::new(config);

        let streams = vec![
            mock_stream(1, 0, 0, 1920, 1080),
            mock_stream(2, 0, 0, 1920, 1080),
        ];
        manager.initialize_from_streams(&streams).await.unwrap();

        let layout = manager.get_layout().await.unwrap();
        assert_eq!(layout.virtual_desktop.width, 1920);
        assert_eq!(layout.virtual_desktop.height, 2160);
    }

    #[tokio::test]
    async fn test_empty_monitor_count() {
        let config = MultiMonitorConfig::default();
        let manager = Monitors::new(config);

        assert_eq!(manager.monitor_count().await, 0);
    }

    #[tokio::test]
    async fn test_mixed_resolution_monitors() {
        let config = MultiMonitorConfig::default();
        let manager = Monitors::new(config);

        // Simulate a 4K primary with a 1080p secondary
        let streams = vec![
            mock_stream(1, 0, 0, 3840, 2160),
            mock_stream(2, 3840, 0, 1920, 1080),
        ];
        manager.initialize_from_streams(&streams).await.unwrap();

        let layout = manager.get_layout().await.unwrap();
        assert_eq!(layout.virtual_desktop.width, 5760);
        assert_eq!(layout.virtual_desktop.height, 2160);
    }

    #[tokio::test]
    async fn test_negative_position_monitors() {
        let config = MultiMonitorConfig::default();
        let manager = Monitors::new(config);

        let streams = vec![
            mock_stream(1, 0, 0, 1920, 1080),
            mock_stream(2, -1920, 0, 1920, 1080),
        ];
        manager.initialize_from_streams(&streams).await.unwrap();

        let layout = manager.get_layout().await.unwrap();
        assert_eq!(layout.virtual_desktop.width, 3840);
        assert_eq!(layout.virtual_desktop.offset_x, -1920);
    }
}
