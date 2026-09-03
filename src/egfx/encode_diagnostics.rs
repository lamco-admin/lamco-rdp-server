//! Encode-side diagnostic tooling — answers "is our H.264 encoding actually valid?"
//!
//! This module collects three complementary capabilities into one place so the
//! recurring question "did our encoder produce bytes a decoder will accept?"
//! can be answered from a single log/test run instead of inferred over many
//! turns of investigation.
//!
//! Option A — `log_nal_hex_dump`: TRACE-level per-NAL hex dump (always-on
//!   when the `lamco_rdp_server::egfx::encoder` tracing target is at TRACE
//!   level). Surfaces structural anomalies — e.g. a 4-byte PPS, a missing
//!   start code, an unexpected NAL type — before they reach the wire.
//!
//! Option B — `dump_frame`: append raw encoded bytes (Annex B) to a single
//!   file specified by config. The output is a valid H.264 elementary stream
//!   that any decoder (`ffmpeg -i frames.h264 -f null -`, `ffprobe`, etc.)
//!   can ingest and report errors on with frame numbers and NAL details.
//!   Off by default; one config flag turns it on.
//!
//! Option D — `self_test`: feed the encoded bytes back through an in-process
//!   OpenH264 decoder. If our own decoder rejects what we just produced,
//!   the bytes are not valid H.264 — regardless of what any RDP client says.
//!   Off by default; one config flag turns it on.
//!
//! Construction is fallible (file open, decoder init) and is propagated as
//! anyhow::Error. Once constructed, the per-frame methods are infallible from
//! the encoder's perspective — failures are logged WARN and the encoder
//! proceeds. The encoder is never blocked by a diagnostic.

use std::{
    fs::{File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::Mutex,
};

use anyhow::{Context, Result};
use tracing::{debug, trace, warn};

/// Per-instance diagnostic state. Cheap to clone via Arc by the caller.
pub struct EncodeDiagnostics {
    /// When set, every frame's encoded bytes are appended here as raw Annex B.
    dump_file: Option<Mutex<File>>,
    /// Path retained for log messages.
    dump_path: Option<PathBuf>,
    /// When set, every frame is decoded by an in-process OpenH264 decoder
    /// and any failure is logged WARN. Decoder is wrapped in Mutex because
    /// it carries internal state and cannot be shared across threads safely
    /// without serialization.
    self_test_decoder: Option<Mutex<self_test::Decoder>>,
    /// Monotonically increasing identifier for log correlation, regardless of
    /// the encoder's own frame counter.
    invocation_counter: std::sync::atomic::AtomicU64,
}

impl EncodeDiagnostics {
    /// Construct from config flags.
    ///
    /// - `dump_path = Some(p)` opens (creates / appends) `p` for raw frame writes.
    /// - `decode_self_test = true` initializes the in-process decoder.
    ///
    /// Either or both may be enabled independently.
    pub fn new(dump_path: Option<&Path>, decode_self_test: bool) -> Result<Self> {
        let (dump_file, dump_path_owned) = match dump_path {
            Some(p) => {
                let f = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(p)
                    .with_context(|| format!("opening H.264 dump file: {}", p.display()))?;
                (Some(Mutex::new(f)), Some(p.to_path_buf()))
            }
            None => (None, None),
        };

        let self_test_decoder = if decode_self_test {
            Some(Mutex::new(
                self_test::Decoder::new().context("initializing OpenH264 decoder for self-test")?,
            ))
        } else {
            None
        };

        Ok(Self {
            dump_file,
            dump_path: dump_path_owned,
            self_test_decoder,
            invocation_counter: std::sync::atomic::AtomicU64::new(0),
        })
    }

    /// True when at least one diagnostic is active. Used by callers to skip
    /// the entire diagnostic block when nothing is enabled.
    pub fn is_active(&self) -> bool {
        self.dump_file.is_some() || self.self_test_decoder.is_some()
    }

    /// Describe enabled options for startup logging.
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if let Some(p) = &self.dump_path {
            parts.push(format!("dump={}", p.display()));
        }
        if self.self_test_decoder.is_some() {
            parts.push("self_test=on".to_string());
        }
        if parts.is_empty() {
            "disabled".to_string()
        } else {
            parts.join(", ")
        }
    }

    /// Append encoded frame bytes to the dump file (Option B).
    pub fn dump_frame(&self, bytes: &[u8]) {
        if let Some(mtx) = &self.dump_file {
            let mut guard = match mtx.lock() {
                Ok(g) => g,
                Err(poisoned) => {
                    warn!("encode_diagnostics dump_file mutex poisoned; recovering");
                    poisoned.into_inner()
                }
            };
            if let Err(e) = guard.write_all(bytes) {
                warn!("encode_diagnostics dump_frame write failed: {e:#}");
            }
        }
    }

    /// Decode the encoded bytes through an in-process OpenH264 decoder
    /// (Option D). Logs WARN on failure with context. `label` identifies
    /// which stream (e.g. "AVC420", "AVC444-Main", "AVC444-Aux").
    pub fn self_test(&self, bytes: &[u8], label: &str) {
        let Some(mtx) = &self.self_test_decoder else {
            return;
        };
        let id = self
            .invocation_counter
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut guard = match mtx.lock() {
            Ok(g) => g,
            Err(poisoned) => {
                warn!("encode_diagnostics self_test mutex poisoned; recovering");
                poisoned.into_inner()
            }
        };
        match guard.decode(bytes) {
            Ok(self_test::DecodeOutcome::Picture { width, height }) => {
                trace!(
                    diag_id = id,
                    label,
                    bytes_len = bytes.len(),
                    width,
                    height,
                    "Encoder self-test: decoder accepted, picture produced",
                );
            }
            Ok(self_test::DecodeOutcome::NoPicture) => {
                // Some NAL sequences (lone SPS, lone PPS) produce no picture
                // but ARE valid decoder input. Logged at DEBUG, not WARN.
                debug!(
                    diag_id = id,
                    label,
                    bytes_len = bytes.len(),
                    "Encoder self-test: decoder accepted bytes but produced no picture (likely parameter-set-only)",
                );
            }
            Err(e) => {
                warn!(
                    diag_id = id,
                    label,
                    bytes_len = bytes.len(),
                    "Encoder self-test: OUR OWN DECODER REJECTED encoded bytes: {e}",
                );
            }
        }
    }
}

/// TRACE-level per-NAL hex dump (Option A — always-on when target is at TRACE).
///
/// Parses the Annex B stream, splits at start codes, and for each NAL emits
/// type + length + first 32 bytes + last 16 bytes hex. Cheap to call even
/// when TRACE is disabled: the `tracing::trace_enabled!` short-circuits the
/// formatting work.
pub(crate) fn log_nal_hex_dump(data: &[u8], frame_num: u64, label: &str) {
    if !tracing::enabled!(tracing::Level::TRACE) {
        return;
    }

    let nals = parse_annex_b_nals(data);
    for (idx, (nal_type, nal_data)) in nals.iter().enumerate() {
        let head = hex_preview(nal_data, 32);
        let tail = if nal_data.len() > 48 {
            hex_preview(&nal_data[nal_data.len().saturating_sub(16)..], 16)
        } else {
            String::from("(short)")
        };
        trace!(
            frame = frame_num,
            label,
            nal_idx = idx,
            nal_type = *nal_type,
            nal_type_name = nal_type_name(*nal_type),
            nal_size = nal_data.len(),
            head_hex = %head,
            tail_hex = %tail,
            "NAL"
        );
    }
}

/// Parse an Annex B stream into a list of (nal_type, nal_bytes_without_start_code).
fn parse_annex_b_nals(data: &[u8]) -> Vec<(u8, &[u8])> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < data.len() {
        let sc_len = if i + 4 <= data.len() && data[i..i + 4] == [0x00, 0x00, 0x00, 0x01] {
            4
        } else if i + 3 <= data.len() && data[i..i + 3] == [0x00, 0x00, 0x01] {
            3
        } else {
            i += 1;
            continue;
        };
        let nal_start = i + sc_len;
        if nal_start >= data.len() {
            break;
        }
        let nal_type = data[nal_start] & 0x1F;
        let mut nal_end = data.len();
        let mut j = nal_start + 1;
        while j + 2 < data.len() {
            if data[j..j + 3] == [0x00, 0x00, 0x01]
                || (j + 3 < data.len() && data[j..j + 4] == [0x00, 0x00, 0x00, 0x01])
            {
                nal_end = j;
                break;
            }
            j += 1;
        }
        out.push((nal_type, &data[nal_start..nal_end]));
        i = nal_end;
    }
    out
}

fn nal_type_name(t: u8) -> &'static str {
    match t {
        1 => "P-slice",
        2 => "B-slice",
        5 => "IDR",
        6 => "SEI",
        7 => "SPS",
        8 => "PPS",
        9 => "AUD",
        _ => "Other",
    }
}

fn hex_preview(data: &[u8], n: usize) -> String {
    let take = data.len().min(n);
    let mut s = String::with_capacity(take * 3);
    for (i, b) in data.iter().take(take).enumerate() {
        if i > 0 {
            s.push(' ');
        }
        s.push_str(&format!("{b:02x}"));
    }
    s
}

mod self_test {
    //! Thin wrapper around the openh264 decoder so the parent module doesn't
    //! have to know about the crate's specific API surface. Returning the
    //! coarse "did it decode" answer keeps the contract simple.

    use anyhow::Result;

    #[cfg(feature = "h264")]
    pub(super) struct Decoder {
        inner: openh264::decoder::Decoder,
    }

    #[cfg(feature = "h264")]
    pub(super) enum DecodeOutcome {
        Picture { width: usize, height: usize },
        NoPicture,
    }

    #[cfg(feature = "h264")]
    #[expect(
        unsafe_code,
        reason = "openh264_sys2::DynamicAPI::from_blob_path_unchecked is unsafe by design \
                  (cannot verify a dynamically-loaded shared library matches the bindings). \
                  We use the same system libopenh264 the encoder already loads successfully, \
                  so the unsafety is contained: any ABI mismatch surfaces at init time, not later."
    )]
    impl Decoder {
        pub(super) fn new() -> Result<Self> {
            // The openh264 crate exposes `Decoder::new()` only when built with
            // the `source` feature (Cisco's source bundled at compile time).
            // We build with `libloading` against the system libopenh264, so
            // we need `with_api_config` + a `DynamicAPI` loaded from a known
            // path. Try the common Linux distro paths in order; first success
            // wins. If none load, the self-test simply isn't available.
            //
            // SAFETY note: `from_blob_path_unchecked` is `unsafe` because the
            // crate cannot verify the binary matches the bindings it generated
            // against. We use the SAME library the encoder loaded successfully,
            // so the unsafety is contained.
            use openh264::decoder::{Decoder as Inner, DecoderConfig};
            use openh264_sys2::DynamicAPI;

            const CANDIDATES: &[&str] = &[
                "/usr/lib64/libopenh264.so",
                "/usr/lib/x86_64-linux-gnu/libopenh264.so",
                "/usr/lib/libopenh264.so",
                "/usr/lib64/libopenh264.so.7",
                "/usr/lib/x86_64-linux-gnu/libopenh264.so.7",
            ];

            let mut last_err: Option<String> = None;
            for path in CANDIDATES {
                // SAFETY: only called for paths we expect to be system-provided
                // OpenH264 builds. Any mismatch would surface immediately as
                // an init error rather than UB at decode time, because the
                // first call enters the OpenH264 ABI which validates symbols.
                let api_result = unsafe { DynamicAPI::from_blob_path_unchecked(path) };
                match api_result {
                    Ok(api) => match Inner::with_api_config(api, DecoderConfig::new()) {
                        Ok(inner) => return Ok(Self { inner }),
                        Err(e) => {
                            last_err = Some(format!("decoder init at {path} failed: {e}"));
                        }
                    },
                    Err(e) => {
                        last_err = Some(format!("library load at {path} failed: {e}"));
                    }
                }
            }
            anyhow::bail!(
                "self-test decoder: no OpenH264 library could be loaded ({}). Tried: {:?}",
                last_err.unwrap_or_else(|| "unknown".to_string()),
                CANDIDATES
            )
        }

        pub(super) fn decode(&mut self, bytes: &[u8]) -> Result<DecodeOutcome> {
            // The crate's `decode()` returns Result<Option<DecodedYUV>>. None
            // means "decoder consumed bytes but produced no picture" (e.g.
            // lone parameter-set NALs), which is NOT a failure — just no
            // output yet. We report picture dimensions via dimensions_uv()
            // doubled, since the public surface only exposes UV plane dims.
            match self.inner.decode(bytes) {
                Ok(Some(picture)) => {
                    let (uv_w, uv_h) = picture.dimensions_uv();
                    // For YUV420, picture luma dims are 2x the UV plane dims.
                    Ok(DecodeOutcome::Picture {
                        width: uv_w * 2,
                        height: uv_h * 2,
                    })
                }
                Ok(None) => Ok(DecodeOutcome::NoPicture),
                Err(e) => Err(anyhow::anyhow!("decoder rejected: {e}")),
            }
        }
    }

    // Without the h264 feature, the encoder isn't compiled either, so this
    // module is never instantiated. The stub below keeps cfg() noise out of
    // the parent module.
    #[cfg(not(feature = "h264"))]
    pub struct Decoder;

    #[cfg(not(feature = "h264"))]
    pub enum DecodeOutcome {
        Picture { width: usize, height: usize },
        NoPicture,
    }

    #[cfg(not(feature = "h264"))]
    impl Decoder {
        pub fn new() -> Result<Self> {
            anyhow::bail!("self-test decoder requires the h264 feature");
        }
        pub fn decode(&mut self, _bytes: &[u8]) -> Result<DecodeOutcome> {
            unreachable!()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_three_nals_with_4byte_start_codes() {
        let stream: Vec<u8> = vec![
            0x00, 0x00, 0x00, 0x01, 0x67, 0xAA, 0xBB, // SPS (type 7)
            0x00, 0x00, 0x00, 0x01, 0x68, 0xCC, // PPS (type 8)
            0x00, 0x00, 0x00, 0x01, 0x65, 0xDD, 0xEE, 0xFF, // IDR (type 5)
        ];
        let nals = parse_annex_b_nals(&stream);
        assert_eq!(nals.len(), 3);
        assert_eq!(nals[0].0, 7);
        assert_eq!(nals[1].0, 8);
        assert_eq!(nals[2].0, 5);
    }

    #[test]
    fn parse_handles_mixed_3_and_4_byte_start_codes() {
        let stream: Vec<u8> = vec![
            0x00, 0x00, 0x01, 0x67, 0xAA, // SPS with 3-byte start
            0x00, 0x00, 0x00, 0x01, 0x68, 0xBB, // PPS with 4-byte start
        ];
        let nals = parse_annex_b_nals(&stream);
        assert_eq!(nals.len(), 2);
        assert_eq!(nals[0].0, 7);
        assert_eq!(nals[1].0, 8);
    }

    #[test]
    fn empty_stream_yields_no_nals() {
        let nals = parse_annex_b_nals(&[]);
        assert!(nals.is_empty());
    }

    #[test]
    fn diagnostics_disabled_by_default() {
        let d = EncodeDiagnostics::new(None, false).unwrap();
        assert!(!d.is_active());
        assert_eq!(d.summary(), "disabled");
    }

    #[test]
    fn dump_file_writes_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("frames.h264");
        let d = EncodeDiagnostics::new(Some(&path), false).unwrap();
        assert!(d.is_active());
        d.dump_frame(&[0x00, 0x00, 0x00, 0x01, 0x67]);
        d.dump_frame(&[0x00, 0x00, 0x00, 0x01, 0x68]);
        drop(d);
        let content = std::fs::read(&path).unwrap();
        assert_eq!(
            content,
            vec![0x00, 0x00, 0x00, 0x01, 0x67, 0x00, 0x00, 0x00, 0x01, 0x68]
        );
    }
}
