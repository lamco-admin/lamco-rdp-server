//! Send-safe proxy to a hardware H.264 encoder running on a dedicated thread.
//!
//! Hardware encoders (VA-API especially) hold thread-affine, `!Send` GPU
//! handles, but the display pipeline is a `tokio::spawn`'d future that must be
//! `Send`. This proxy creates the encoder on, and never moves it off, one
//! dedicated OS thread, exchanging frames over channels. The proxy holds only
//! channels, so it is `Send`.
//!
//! Requests are serviced strictly in order on a single channel, so callers that
//! need a shared DPB across sequential encodes (the AVC444 main/aux pair) get it
//! for free by issuing those encodes back to back.

use std::sync::mpsc;

use super::create_hardware_encoder;
use crate::config::HardwareEncodingConfig;

/// A frame to encode, plus how it should be encoded.
enum Request {
    /// BGRA input; the encoder does its own color conversion (AVC420 path).
    Bgra {
        data: Vec<u8>,
        width: u32,
        height: u32,
        timestamp_ms: u64,
    },
    /// Pre-formed NV12 with an explicit IDR decision (AVC444 subframe path).
    Nv12 {
        data: Vec<u8>,
        width: u32,
        height: u32,
        timestamp_ms: u64,
        force_keyframe: bool,
    },
    /// Force the next encoded frame to be an IDR.
    ForceKeyframe,
}

/// `Ok(Some((bitstream, is_keyframe)))`, `Ok(None)` when the encoder skipped the
/// frame (rate control), or `Err(message)` on failure.
type Response = Result<Option<(Vec<u8>, bool)>, String>;

/// Handle to a hardware encoder living on its own thread.
///
/// Dropping the handle drops the request sender, which ends the thread's receive
/// loop and lets the encoder be destroyed on its own thread.
pub struct HardwareEncoderThread {
    request_tx: mpsc::Sender<Request>,
    response_rx: mpsc::Receiver<Response>,
    backend_name: &'static str,
}

impl HardwareEncoderThread {
    /// Spawn the thread and build a hardware encoder at `(width, height)`.
    /// Blocks until the encoder is constructed, so a build failure is reported
    /// here (and the caller can fall back to software) rather than mid-stream.
    pub fn spawn(
        hw_config: &HardwareEncodingConfig,
        width: u32,
        height: u32,
    ) -> Result<Self, String> {
        let hw_config = hw_config.clone();
        let (request_tx, request_rx) = mpsc::channel::<Request>();
        let (response_tx, response_rx) = mpsc::channel::<Response>();
        let (init_tx, init_rx) = mpsc::channel::<Result<&'static str, String>>();

        std::thread::Builder::new()
            .name("lamco-hw-encoder".to_string())
            .spawn(move || {
                let mut encoder = match create_hardware_encoder(&hw_config, width, height) {
                    Ok(enc) => {
                        let _ = init_tx.send(Ok(enc.backend_name()));
                        enc
                    }
                    Err(e) => {
                        let _ = init_tx.send(Err(e.to_string()));
                        return;
                    }
                };

                while let Ok(req) = request_rx.recv() {
                    let response: Response = match req {
                        Request::Bgra {
                            data,
                            width,
                            height,
                            timestamp_ms,
                        } => encoder
                            .encode_bgra(&data, width, height, timestamp_ms)
                            .map(|opt| opt.map(|f| (f.data, f.is_keyframe)))
                            .map_err(|e| e.to_string()),
                        Request::Nv12 {
                            data,
                            width,
                            height,
                            timestamp_ms,
                            force_keyframe,
                        } => encoder
                            .encode_nv12(&data, width, height, timestamp_ms, force_keyframe)
                            .map(|opt| opt.map(|f| (f.data, f.is_keyframe)))
                            .map_err(|e| e.to_string()),
                        Request::ForceKeyframe => {
                            encoder.force_keyframe();
                            continue;
                        }
                    };
                    if response_tx.send(response).is_err() {
                        break;
                    }
                }
            })
            .map_err(|e| format!("failed to spawn hardware encoder thread: {e}"))?;

        let backend_name = init_rx
            .recv()
            .map_err(|_| "hardware encoder thread exited during init".to_string())??;

        Ok(Self {
            request_tx,
            response_rx,
            backend_name,
        })
    }

    /// Backend that was actually built (e.g. `"vaapi"`).
    pub fn backend_name(&self) -> &'static str {
        self.backend_name
    }

    /// Encode a BGRA frame (AVC420). Blocks until the encoder responds.
    pub fn encode_bgra(
        &self,
        data: Vec<u8>,
        width: u32,
        height: u32,
        timestamp_ms: u64,
    ) -> Response {
        self.request_tx
            .send(Request::Bgra {
                data,
                width,
                height,
                timestamp_ms,
            })
            .map_err(|_| "hardware encoder thread is gone".to_string())?;
        self.recv()
    }

    /// Encode a pre-formed NV12 frame with an explicit IDR decision (AVC444).
    pub fn encode_nv12(
        &self,
        data: Vec<u8>,
        width: u32,
        height: u32,
        timestamp_ms: u64,
        force_keyframe: bool,
    ) -> Response {
        self.request_tx
            .send(Request::Nv12 {
                data,
                width,
                height,
                timestamp_ms,
                force_keyframe,
            })
            .map_err(|_| "hardware encoder thread is gone".to_string())?;
        self.recv()
    }

    /// Force the next encoded frame to be an IDR. Fire-and-forget: no response is
    /// expected, keeping the request/response channels one-to-one for encodes.
    pub fn force_keyframe(&self) {
        let _ = self.request_tx.send(Request::ForceKeyframe);
    }

    fn recv(&self) -> Response {
        self.response_rx
            .recv()
            .map_err(|_| "hardware encoder thread is gone".to_string())?
    }
}
