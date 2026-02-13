//! Mutter ScreenCast D-Bus Interface
//!
//! Defines proxies for org.gnome.Mutter.ScreenCast D-Bus interfaces.
//! These are GNOME-specific and bypass the XDG Portal permission model.

use std::collections::HashMap;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use zbus::{
    zvariant::{OwnedObjectPath, OwnedValue, Value},
    Connection,
};

/// Main ScreenCast interface proxy
///
/// Service: org.gnome.Mutter.ScreenCast
/// Path: /org/gnome/Mutter/ScreenCast
#[derive(Debug)]
pub struct MutterScreenCast<'a> {
    connection: Connection,
    proxy: zbus::Proxy<'a>,
}

impl MutterScreenCast<'_> {
    /// Create a new ScreenCast proxy
    pub async fn new(connection: &Connection) -> Result<Self> {
        let proxy = zbus::ProxyBuilder::new(connection)
            .interface("org.gnome.Mutter.ScreenCast")?
            .path("/org/gnome/Mutter/ScreenCast")?
            .destination("org.gnome.Mutter.ScreenCast")?
            .build()
            .await
            .context("Failed to create Mutter ScreenCast proxy")?;

        Ok(Self {
            connection: connection.clone(),
            proxy,
        })
    }

    /// Create a new screen cast session
    ///
    /// # Arguments
    ///
    /// * `properties` - Session properties (can be empty)
    ///
    /// # Returns
    ///
    /// Object path to the created session
    pub async fn create_session(
        &self,
        properties: HashMap<String, Value<'_>>,
    ) -> Result<OwnedObjectPath> {
        let response = self
            .proxy
            .call_method("CreateSession", &(properties,))
            .await
            .context("Failed to call CreateSession")?;

        let body = response.body();
        let path: OwnedObjectPath = body
            .deserialize()
            .context("Failed to deserialize CreateSession response")?;

        Ok(path)
    }
}

/// ScreenCast Session interface proxy
///
/// Interface: org.gnome.Mutter.ScreenCast.Session
/// Path: /org/gnome/Mutter/ScreenCast/Session/*
#[derive(Debug)]
pub struct MutterScreenCastSession<'a> {
    connection: Connection,
    proxy: zbus::Proxy<'a>,
}

impl MutterScreenCastSession<'_> {
    /// Create a session proxy for an existing session
    pub async fn new(connection: &Connection, session_path: OwnedObjectPath) -> Result<Self> {
        let proxy = zbus::ProxyBuilder::new(connection)
            .interface("org.gnome.Mutter.ScreenCast.Session")?
            .path(session_path)?
            .destination("org.gnome.Mutter.ScreenCast")?
            .build()
            .await
            .context("Failed to create ScreenCast Session proxy")?;

        Ok(Self {
            connection: connection.clone(),
            proxy,
        })
    }

    /// Record a specific monitor
    ///
    /// # Arguments
    ///
    /// * `connector` - Monitor connector name (e.g., "HDMI-1", "eDP-1")
    /// * `properties` - Recording properties (cursor mode, etc.)
    ///
    /// # Returns
    ///
    /// Object path to the stream
    pub async fn record_monitor(
        &self,
        connector: &str,
        properties: HashMap<String, Value<'_>>,
    ) -> Result<OwnedObjectPath> {
        let response = self
            .proxy
            .call_method("RecordMonitor", &(connector, properties))
            .await
            .context("Failed to call RecordMonitor")?;

        let body = response.body();
        let path: OwnedObjectPath = body
            .deserialize()
            .context("Failed to deserialize RecordMonitor response")?;

        Ok(path)
    }

    /// Record a virtual monitor (for headless operation)
    ///
    /// # Arguments
    ///
    /// * `properties` - Recording properties (cursor mode, resolution, etc.)
    ///
    /// # Returns
    ///
    /// Object path to the stream
    pub async fn record_virtual(
        &self,
        properties: HashMap<String, Value<'_>>,
    ) -> Result<OwnedObjectPath> {
        let response = self
            .proxy
            .call_method("RecordVirtual", &(properties,))
            .await
            .context("Failed to call RecordVirtual")?;

        let body = response.body();
        let path: OwnedObjectPath = body
            .deserialize()
            .context("Failed to deserialize RecordVirtual response")?;

        Ok(path)
    }

    /// Start the recording session
    pub async fn start(&self) -> Result<()> {
        self.proxy
            .call_method("Start", &())
            .await
            .context("Failed to call Start")?;

        Ok(())
    }

    /// Stop the recording session
    pub async fn stop(&self) -> Result<()> {
        self.proxy
            .call_method("Stop", &())
            .await
            .context("Failed to call Stop")?;

        Ok(())
    }
}

/// ScreenCast Stream interface proxy
///
/// Interface: org.gnome.Mutter.ScreenCast.Stream
/// Path: /org/gnome/Mutter/ScreenCast/Stream/*
#[derive(Debug)]
pub struct MutterScreenCastStream<'a> {
    proxy: zbus::Proxy<'a>,
}

impl MutterScreenCastStream<'_> {
    /// Create a stream proxy for an existing stream
    pub async fn new(connection: &Connection, stream_path: OwnedObjectPath) -> Result<Self> {
        let proxy = zbus::ProxyBuilder::new(connection)
            .interface("org.gnome.Mutter.ScreenCast.Stream")?
            .path(stream_path)?
            .destination("org.gnome.Mutter.ScreenCast")?
            .build()
            .await
            .context("Failed to create ScreenCast Stream proxy")?;

        Ok(Self { proxy })
    }

    /// Get the PipeWire node ID for this stream by subscribing to PipeWireStreamAdded signal
    ///
    /// The node ID is emitted via the PipeWireStreamAdded signal when the stream starts,
    /// not as a property. We must subscribe before the signal is emitted.
    pub async fn subscribe_for_node_id(
        &self,
    ) -> Result<impl futures_util::Stream<Item = zbus::Message>> {
        self.proxy
            .receive_signal("PipeWireStreamAdded")
            .await
            .context("Failed to subscribe to PipeWireStreamAdded signal")
    }

    /// Get stream parameters (resolution, position, etc.)
    pub async fn parameters(&self) -> Result<StreamParameters> {
        let params: HashMap<String, OwnedValue> = self
            .proxy
            .get_property("Parameters")
            .await
            .context("Failed to get Parameters property")?;

        Ok(StreamParameters::from_dict(params))
    }
}

/// Stream parameters from Mutter
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamParameters {
    /// Stream width in pixels
    pub width: Option<i32>,
    /// Stream height in pixels
    pub height: Option<i32>,
    /// X position
    pub position_x: Option<i32>,
    /// Y position
    pub position_y: Option<i32>,
}

impl StreamParameters {
    fn from_dict(dict: HashMap<String, OwnedValue>) -> Self {
        // Parse Mutter's parameter dictionary with proper zvariant handling
        // Mutter provides: "size" as (ii), "position" as (ii)

        let width = Self::parse_struct_tuple_i32(&dict, "size", 0);
        let height = Self::parse_struct_tuple_i32(&dict, "size", 1);
        let position_x = Self::parse_struct_tuple_i32(&dict, "position", 0);
        let position_y = Self::parse_struct_tuple_i32(&dict, "position", 1);

        Self {
            width,
            height,
            position_x,
            position_y,
        }
    }

    /// Parse a tuple field from a zvariant Structure
    ///
    /// Mutter returns tuples like (width, height) as zvariant::Structure.
    /// This helper rigorously extracts and validates the i32 value at a given index.
    fn parse_struct_tuple_i32(
        dict: &HashMap<String, OwnedValue>,
        key: &str,
        index: usize,
    ) -> Option<i32> {
        use zbus::zvariant::Structure;

        dict.get(key).and_then(|value| {
            match value.downcast_ref::<Structure>() {
                Ok(structure) => {
                    let fields = structure.fields();

                    fields
                        .get(index)
                        .and_then(|field: &zbus::zvariant::Value<'_>| {
                            match field.downcast_ref::<i32>() {
                                Ok(val) => Some(val),
                                Err(_) => {
                                    // Log unexpected type for debugging
                                    tracing::debug!(
                                        "Mutter parameter '{}' index {} has unexpected type: {:?}",
                                        key,
                                        index,
                                        field.value_signature()
                                    );
                                    None
                                }
                            }
                        })
                }
                Err(_) => {
                    // Not a structure, log for debugging
                    tracing::debug!(
                        "Mutter parameter '{}' is not a Structure: {:?}",
                        key,
                        value.value_signature()
                    );
                    None
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore] // Requires GNOME with Mutter
    async fn test_mutter_screencast_availability() {
        match zbus::Connection::session().await {
            Ok(conn) => match MutterScreenCast::new(&conn).await {
                Ok(_proxy) => println!("Mutter ScreenCast API available"),
                Err(e) => println!("Mutter ScreenCast not available: {}", e),
            },
            Err(e) => println!("D-Bus session not available: {}", e),
        }
    }
}
