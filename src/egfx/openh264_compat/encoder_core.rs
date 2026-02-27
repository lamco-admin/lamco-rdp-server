#![expect(unsafe_code, reason = "FFI calls through OpenH264 vtable pointers")]
// Raw pointer casting is fundamental to C FFI: OpenH264's vtable functions
// accept `*const c_void` / `*mut c_void` for type-erased struct parameters.
#![expect(
    clippy::ptr_cast_constness,
    clippy::borrow_as_ptr,
    clippy::ref_as_ptr,
    reason = "C FFI requires raw pointer casts through void pointers"
)]

//! Version-dispatched OpenH264 encoder.
//!
//! Wraps the raw FFI encoder with version-appropriate struct layouts.
//! All version-specific logic is contained here; callers use the
//! unified `VersionedEncoder` API.

use std::{
    os::raw::{c_int, c_void},
    ptr::{addr_of_mut, null_mut},
    sync::Arc,
};

use tracing::{debug, info, warn};

use super::{
    ffi_types::{
        abi7, abi8, EParameterSetStrategy, EUsageType, EncodedFrameData, ISVCEncoder,
        ISVCEncoderVtbl, SSpatialLayerConfig, CONSTANT_ID, DEBLOCKING_IDC_0, ECOMPLEXITY_MODE,
        ENCODER_OPTION_DATAFORMAT, ENCODER_OPTION_SVC_ENCODE_PARAM_EXT, ENCODER_OPTION_TRACE_LEVEL,
        LOW_COMPLEXITY, RC_MODES, RC_QUALITY_MODE, SCREEN_CONTENT_REAL_TIME, SM_SINGLE_SLICE,
        VIDEO_FORMAT_I420, WELS_LOG_QUIET, WELS_LOG_WARNING,
    },
    loader::{AbiGeneration, OpenH264Api},
};

/// VUI (Video Usability Information) configuration for H.264 SPS signaling.
///
/// When enabled, these parameters are written into the SSpatialLayerConfig
/// to signal color space interpretation to the decoder.
#[derive(Debug, Clone, Default)]
pub(crate) struct VuiConfig {
    pub enabled: bool,
    pub full_range: bool,
    pub colour_primaries: u8,
    pub transfer_characteristics: u8,
    pub matrix_coefficients: u8,
}

impl VuiConfig {
    pub(crate) fn bt709_full() -> Self {
        Self {
            enabled: true,
            full_range: true,
            colour_primaries: 1,         // BT.709
            transfer_characteristics: 1, // BT.709
            matrix_coefficients: 1,      // BT.709
        }
    }

    pub(crate) fn bt709() -> Self {
        Self {
            enabled: true,
            full_range: false,
            colour_primaries: 1,
            transfer_characteristics: 1,
            matrix_coefficients: 1,
        }
    }

    pub(crate) fn bt601() -> Self {
        Self {
            enabled: true,
            full_range: false,
            colour_primaries: 6,         // BT.601
            transfer_characteristics: 6, // BT.601
            matrix_coefficients: 6,      // BT.601
        }
    }
}

/// Configuration for the encoder, independent of ABI version.
#[derive(Debug, Clone)]
pub(crate) struct EncoderConfig {
    pub bitrate_bps: u32,
    pub max_frame_rate: f32,
    pub usage_type: EUsageType,
    pub num_threads: u16,
    pub enable_skip_frame: bool,
    pub enable_denoise: bool,
    pub enable_scene_change_detect: bool,
    pub enable_background_detection: bool,
    pub enable_adaptive_quant: bool,
    pub enable_long_term_reference: bool,
    pub complexity: ECOMPLEXITY_MODE,
    pub max_qp: i32,
    pub min_qp: i32,
    pub debug_log: bool,
    /// H.264 level IDC value (e.g., 31 = Level 3.1). None = library default.
    pub level_idc: Option<i32>,
    /// VUI parameters for color space signaling in the H.264 SPS.
    pub vui: VuiConfig,
    /// Rate control mode. RC_QUALITY_MODE prioritizes visual quality (better for
    /// AVC444 dual-encode where main+aux share one encoder). RC_BITRATE_MODE
    /// targets a specific bitrate but can produce tiny P-frames under the
    /// dual-encode pattern.
    pub rc_mode: RC_MODES,
    /// SPS/PPS ID strategy. CONSTANT_ID keeps the same IDs across frames,
    /// which is required when SPS/PPS is cached and stripped from aux streams.
    pub sps_pps_id_strategy: EParameterSetStrategy,
    /// Intra period (0 = disabled). Controls library-level periodic IDR injection.
    /// Set to 0 to manage IDRs manually via force_intra_frame().
    pub intra_period: u32,
}

impl Default for EncoderConfig {
    fn default() -> Self {
        Self {
            bitrate_bps: 2_000_000,
            max_frame_rate: 30.0,
            usage_type: SCREEN_CONTENT_REAL_TIME,
            num_threads: 0, // auto
            enable_skip_frame: true,
            enable_denoise: false,
            enable_scene_change_detect: true,
            enable_background_detection: true,
            enable_adaptive_quant: true,
            enable_long_term_reference: false,
            complexity: LOW_COMPLEXITY,
            max_qp: 51,
            min_qp: 0,
            debug_log: false,
            level_idc: None,
            vui: VuiConfig::default(),
            rc_mode: RC_QUALITY_MODE,
            sps_pps_id_strategy: CONSTANT_ID,
            intra_period: 0,
        }
    }
}

/// Version-dispatched encoder. Holds the correct SFrameBSInfo variant
/// for the detected ABI and dispatches encode/init calls through the
/// version-stable vtable.
pub(crate) struct VersionedEncoder {
    api: Arc<OpenH264Api>,
    encoder_ptr: *mut ISVCEncoder,
    abi: AbiGeneration,
    config: EncoderConfig,
    previous_dimensions: Option<(i32, i32)>,
    // Version-specific frame output buffers
    frame_info_abi7: Option<Box<abi7::SFrameBSInfo>>,
    frame_info_abi8: Option<Box<abi8::SFrameBSInfo>>,
}

// The raw encoder pointer is thread-safe per OpenH264 docs.
unsafe impl Send for VersionedEncoder {}

impl VersionedEncoder {
    /// Create a new versioned encoder from a loaded API.
    pub(crate) fn new(api: Arc<OpenH264Api>, config: EncoderConfig) -> Result<Self, String> {
        let encoder_ptr = unsafe { api.create_encoder_instance()? };
        let abi = api.capabilities.abi;

        let (frame_info_abi7, frame_info_abi8) = match abi {
            AbiGeneration::Abi7 => (Some(Box::new(abi7::SFrameBSInfo::default())), None),
            AbiGeneration::Abi8 => (None, Some(Box::new(abi8::SFrameBSInfo::default()))),
        };

        Ok(Self {
            api,
            encoder_ptr,
            abi,
            config,
            previous_dimensions: None,
            frame_info_abi7,
            frame_info_abi8,
        })
    }

    /// Encode a YUV I420 frame. Handles resolution changes automatically.
    #[expect(
        clippy::too_many_arguments,
        reason = "YUV planes + dimensions + encoding params"
    )]
    pub(crate) fn encode(
        &mut self,
        y: &[u8],
        u: &[u8],
        v: &[u8],
        y_stride: i32,
        u_stride: i32,
        v_stride: i32,
        width: i32,
        height: i32,
        timestamp_ms: i64,
    ) -> Result<EncodedFrameData, String> {
        let new_dims = (width, height);
        if self.previous_dimensions != Some(new_dims) {
            self.reinit(width, height)?;
            self.previous_dimensions = Some(new_dims);
        }

        match self.abi {
            AbiGeneration::Abi7 => self.encode_abi7(
                y,
                u,
                v,
                y_stride,
                u_stride,
                v_stride,
                width,
                height,
                timestamp_ms,
            ),
            AbiGeneration::Abi8 => self.encode_abi8(
                y,
                u,
                v,
                y_stride,
                u_stride,
                v_stride,
                width,
                height,
                timestamp_ms,
            ),
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "ABI-specific encode with YUV planes + dimensions"
    )]
    #[expect(
        clippy::expect_used,
        reason = "frame_info is initialized at encoder creation"
    )]
    fn encode_abi7(
        &mut self,
        y: &[u8],
        u: &[u8],
        v: &[u8],
        y_stride: i32,
        u_stride: i32,
        v_stride: i32,
        width: i32,
        height: i32,
        timestamp_ms: i64,
    ) -> Result<EncodedFrameData, String> {
        let source = abi7::SSourcePicture {
            iColorFormat: VIDEO_FORMAT_I420 as c_int,
            iStride: [y_stride, u_stride, v_stride, 0],
            pData: [
                y.as_ptr() as *mut _,
                u.as_ptr() as *mut _,
                v.as_ptr() as *mut _,
                null_mut(),
            ],
            iPicWidth: width,
            iPicHeight: height,
            uiTimeStamp: timestamp_ms as _,
        };

        // Read vtable before taking mutable borrow on frame_info
        let vtbl = self.vtable()?;
        let encode_fn = vtbl.EncodeFrame.ok_or("EncodeFrame not in vtable")?;

        let frame_info = self
            .frame_info_abi7
            .as_mut()
            .expect("ABI 7 frame info must exist for ABI 7 encoder");

        let ret = unsafe {
            encode_fn(
                self.encoder_ptr,
                &source as *const _ as *const c_void,
                frame_info.as_mut() as *mut _ as *mut c_void,
            )
        };

        if ret != 0 {
            return Err(format!("EncodeFrame returned {ret}"));
        }

        Ok(unsafe { EncodedFrameData::from_abi7(frame_info) })
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "ABI-specific encode with YUV planes + dimensions"
    )]
    #[expect(
        clippy::expect_used,
        reason = "frame_info is initialized at encoder creation"
    )]
    fn encode_abi8(
        &mut self,
        y: &[u8],
        u: &[u8],
        v: &[u8],
        y_stride: i32,
        u_stride: i32,
        v_stride: i32,
        width: i32,
        height: i32,
        timestamp_ms: i64,
    ) -> Result<EncodedFrameData, String> {
        let source = abi8::SSourcePicture {
            iColorFormat: VIDEO_FORMAT_I420 as c_int,
            iStride: [y_stride, u_stride, v_stride, 0],
            pData: [
                y.as_ptr() as *mut _,
                u.as_ptr() as *mut _,
                v.as_ptr() as *mut _,
                null_mut(),
            ],
            iPicWidth: width,
            iPicHeight: height,
            uiTimeStamp: timestamp_ms as _,
            bPsnrY: false,
            bPsnrU: false,
            bPsnrV: false,
        };

        // Read vtable before taking mutable borrow on frame_info
        let vtbl = self.vtable()?;
        let encode_fn = vtbl.EncodeFrame.ok_or("EncodeFrame not in vtable")?;

        let frame_info = self
            .frame_info_abi8
            .as_mut()
            .expect("ABI 8 frame info must exist for ABI 8 encoder");

        let ret = unsafe {
            encode_fn(
                self.encoder_ptr,
                &source as *const _ as *const c_void,
                frame_info.as_mut() as *mut _ as *mut c_void,
            )
        };

        if ret != 0 {
            return Err(format!("EncodeFrame returned {ret}"));
        }

        Ok(unsafe { EncodedFrameData::from_abi8(frame_info) })
    }

    /// Force the next encoded frame to be an IDR keyframe.
    pub(crate) fn force_intra_frame(&mut self) {
        if let Ok(vtbl) = self.vtable() {
            if let Some(force_fn) = vtbl.ForceIntraFrame {
                unsafe { force_fn(self.encoder_ptr, true) };
            }
        }
    }

    /// Initialize or re-initialize the encoder with new dimensions.
    fn reinit(&mut self, width: i32, height: i32) -> Result<(), String> {
        let greater = width.max(height);
        let smaller = width.min(height);
        if greater > 3840 || smaller > 2160 {
            return Err(format!(
                "Resolution {width}x{height} exceeds OpenH264 max 3840x2160"
            ));
        }

        match self.abi {
            AbiGeneration::Abi7 => self.reinit_abi7(width, height),
            AbiGeneration::Abi8 => self.reinit_abi8(width, height),
        }
    }

    fn reinit_abi7(&mut self, width: i32, height: i32) -> Result<(), String> {
        let vtbl = self.vtable()?;
        let mut params = abi7::SEncParamExt::default();

        // Let the library fill defaults at its own expected struct size
        let get_defaults = vtbl
            .GetDefaultParams
            .ok_or("GetDefaultParams not in vtable")?;
        let ret = unsafe { get_defaults(self.encoder_ptr, &mut params as *mut _ as *mut c_void) };
        if ret != 0 {
            return Err(format!("GetDefaultParams failed: {ret}"));
        }

        self.apply_config_to_params_abi7(&mut params, width, height);

        if self.previous_dimensions.is_none() {
            let init_fn = vtbl.InitializeExt.ok_or("InitializeExt not in vtable")?;
            let ret = unsafe { init_fn(self.encoder_ptr, &params as *const _ as *const c_void) };
            if ret != 0 {
                return Err(format!("InitializeExt failed: {ret}"));
            }

            self.set_trace_level(vtbl)?;
            self.set_data_format(vtbl)?;
        } else {
            let set_opt = vtbl.SetOption.ok_or("SetOption not in vtable")?;
            let ret = unsafe {
                set_opt(
                    self.encoder_ptr,
                    ENCODER_OPTION_SVC_ENCODE_PARAM_EXT,
                    &mut params as *mut _ as *mut c_void,
                )
            };
            if ret != 0 {
                return Err(format!("SetOption(SVC_ENCODE_PARAM_EXT) failed: {ret}"));
            }
            self.force_intra_frame();
        }

        info!(
            "OpenH264 encoder initialized: {width}x{height}, {}kbps, {:.0}fps (ABI 7)",
            self.config.bitrate_bps / 1000,
            self.config.max_frame_rate,
        );
        debug!(
            "encoder params: rc_mode={}, sps_pps_id_strategy={}, intra_period={}, \
             usage_type={}, complexity={}, skip_frame={}, scene_change={}, \
             qp_range=[{},{}], deblocking={}",
            self.config.rc_mode,
            self.config.sps_pps_id_strategy,
            self.config.intra_period,
            self.config.usage_type,
            self.config.complexity,
            self.config.enable_skip_frame,
            self.config.enable_scene_change_detect,
            self.config.min_qp,
            self.config.max_qp,
            DEBLOCKING_IDC_0,
        );
        Ok(())
    }

    fn reinit_abi8(&mut self, width: i32, height: i32) -> Result<(), String> {
        let vtbl = self.vtable()?;
        let mut params = abi8::SEncParamExt::default();

        let get_defaults = vtbl
            .GetDefaultParams
            .ok_or("GetDefaultParams not in vtable")?;
        let ret = unsafe { get_defaults(self.encoder_ptr, &mut params as *mut _ as *mut c_void) };
        if ret != 0 {
            return Err(format!("GetDefaultParams failed: {ret}"));
        }

        self.apply_config_to_params_abi8(&mut params, width, height);

        if self.previous_dimensions.is_none() {
            let init_fn = vtbl.InitializeExt.ok_or("InitializeExt not in vtable")?;
            let ret = unsafe { init_fn(self.encoder_ptr, &params as *const _ as *const c_void) };
            if ret != 0 {
                return Err(format!("InitializeExt failed: {ret}"));
            }

            self.set_trace_level(vtbl)?;
            self.set_data_format(vtbl)?;
        } else {
            let set_opt = vtbl.SetOption.ok_or("SetOption not in vtable")?;
            let ret = unsafe {
                set_opt(
                    self.encoder_ptr,
                    ENCODER_OPTION_SVC_ENCODE_PARAM_EXT,
                    &mut params as *mut _ as *mut c_void,
                )
            };
            if ret != 0 {
                return Err(format!("SetOption(SVC_ENCODE_PARAM_EXT) failed: {ret}"));
            }
            self.force_intra_frame();
        }

        info!(
            "OpenH264 encoder initialized: {width}x{height}, {}kbps, {:.0}fps (ABI 8)",
            self.config.bitrate_bps / 1000,
            self.config.max_frame_rate,
        );
        debug!(
            "encoder params: rc_mode={}, sps_pps_id_strategy={}, intra_period={}, \
             usage_type={}, complexity={}, skip_frame={}, scene_change={}, \
             qp_range=[{},{}], deblocking={}",
            self.config.rc_mode,
            self.config.sps_pps_id_strategy,
            self.config.intra_period,
            self.config.usage_type,
            self.config.complexity,
            self.config.enable_skip_frame,
            self.config.enable_scene_change_detect,
            self.config.min_qp,
            self.config.max_qp,
            DEBLOCKING_IDC_0,
        );
        Ok(())
    }

    /// Apply our config to ABI 7 params. All fields before iIdrBitrateRatio
    /// are at identical offsets across both versions.
    fn apply_config_to_params_abi7(&self, p: &mut abi7::SEncParamExt, w: i32, h: i32) {
        self.apply_common_params_abi7(p, w, h);
        self.apply_vui_to_layer(&mut p.sSpatialLayers[0]);
    }

    /// Apply our config to ABI 8 params.
    fn apply_config_to_params_abi8(&self, p: &mut abi8::SEncParamExt, w: i32, h: i32) {
        self.apply_common_params_abi8(p, w, h);
        self.apply_vui_to_layer(&mut p.sSpatialLayers[0]);
    }

    /// Common parameter setup for ABI 7.
    fn apply_common_params_abi7(&self, p: &mut abi7::SEncParamExt, w: i32, h: i32) {
        p.iUsageType = self.config.usage_type;
        p.iPicWidth = w;
        p.iPicHeight = h;
        p.iTargetBitrate = self.config.bitrate_bps as c_int;
        p.iRCMode = self.config.rc_mode;
        p.fMaxFrameRate = self.config.max_frame_rate;
        p.bEnableFrameSkip = self.config.enable_skip_frame;
        p.bEnableDenoise = self.config.enable_denoise;
        p.bEnableSceneChangeDetect = self.config.enable_scene_change_detect;
        p.bEnableBackgroundDetection = self.config.enable_background_detection;
        p.bEnableAdaptiveQuant = self.config.enable_adaptive_quant;
        p.bEnableLongTermReference = self.config.enable_long_term_reference;
        p.iComplexityMode = self.config.complexity;
        p.iMultipleThreadIdc = self.config.num_threads;
        p.iMaxQp = self.config.max_qp;
        p.iMinQp = self.config.min_qp;
        p.eSpsPpsIdStrategy = self.config.sps_pps_id_strategy;
        p.uiIntraPeriod = self.config.intra_period;
        p.iLoopFilterDisableIdc = DEBLOCKING_IDC_0;
        p.iSpatialLayerNum = 1;
        p.iTemporalLayerNum = 1;
        p.iLtrMarkPeriod = 30;
        p.sSpatialLayers[0].iVideoWidth = w;
        p.sSpatialLayers[0].iVideoHeight = h;
        p.sSpatialLayers[0].fFrameRate = self.config.max_frame_rate;
        p.sSpatialLayers[0].iSpatialBitrate = self.config.bitrate_bps as c_int;
        p.sSpatialLayers[0].iMaxSpatialBitrate = self.config.bitrate_bps as c_int;
        p.sSpatialLayers[0].sSliceArgument.uiSliceMode = SM_SINGLE_SLICE;
        p.sSpatialLayers[0].sSliceArgument.uiSliceNum = 1;
        if let Some(level) = self.config.level_idc {
            p.sSpatialLayers[0].uiLevelIdc = level;
        }
    }

    /// Common parameter setup for ABI 8.
    fn apply_common_params_abi8(&self, p: &mut abi8::SEncParamExt, w: i32, h: i32) {
        p.iUsageType = self.config.usage_type;
        p.iPicWidth = w;
        p.iPicHeight = h;
        p.iTargetBitrate = self.config.bitrate_bps as c_int;
        p.iRCMode = self.config.rc_mode;
        p.fMaxFrameRate = self.config.max_frame_rate;
        p.bEnableFrameSkip = self.config.enable_skip_frame;
        p.bEnableDenoise = self.config.enable_denoise;
        p.bEnableSceneChangeDetect = self.config.enable_scene_change_detect;
        p.bEnableBackgroundDetection = self.config.enable_background_detection;
        p.bEnableAdaptiveQuant = self.config.enable_adaptive_quant;
        p.bEnableLongTermReference = self.config.enable_long_term_reference;
        p.iComplexityMode = self.config.complexity;
        p.iMultipleThreadIdc = self.config.num_threads;
        p.iMaxQp = self.config.max_qp;
        p.iMinQp = self.config.min_qp;
        p.eSpsPpsIdStrategy = self.config.sps_pps_id_strategy;
        p.uiIntraPeriod = self.config.intra_period;
        p.iLoopFilterDisableIdc = DEBLOCKING_IDC_0;
        p.iSpatialLayerNum = 1;
        p.iTemporalLayerNum = 1;
        p.iLtrMarkPeriod = 30;
        p.sSpatialLayers[0].iVideoWidth = w;
        p.sSpatialLayers[0].iVideoHeight = h;
        p.sSpatialLayers[0].fFrameRate = self.config.max_frame_rate;
        p.sSpatialLayers[0].iSpatialBitrate = self.config.bitrate_bps as c_int;
        p.sSpatialLayers[0].iMaxSpatialBitrate = self.config.bitrate_bps as c_int;
        p.sSpatialLayers[0].sSliceArgument.uiSliceMode = SM_SINGLE_SLICE;
        p.sSpatialLayers[0].sSliceArgument.uiSliceNum = 1;
        if let Some(level) = self.config.level_idc {
            p.sSpatialLayers[0].uiLevelIdc = level;
        }
    }

    /// Apply VUI configuration to a spatial layer (version-independent: SSpatialLayerConfig
    /// is stable across all versions we support).
    fn apply_vui_to_layer(&self, layer: &mut SSpatialLayerConfig) {
        if !self.config.vui.enabled {
            return;
        }
        layer.bVideoSignalTypePresent = true;
        layer.uiVideoFormat = 5; // "unspecified" per H.264 spec
        layer.bFullRange = self.config.vui.full_range;
        layer.bColorDescriptionPresent = true;
        layer.uiColorPrimaries = self.config.vui.colour_primaries;
        layer.uiTransferCharacteristics = self.config.vui.transfer_characteristics;
        layer.uiColorMatrix = self.config.vui.matrix_coefficients;
    }

    /// Read the vtable from the encoder pointer.
    fn vtable(&self) -> Result<&ISVCEncoderVtbl, String> {
        if self.encoder_ptr.is_null() {
            return Err("Encoder pointer is null".to_string());
        }
        unsafe {
            let vtbl_ptr = *(self.encoder_ptr as *const *const ISVCEncoderVtbl);
            if vtbl_ptr.is_null() {
                return Err("Encoder vtable pointer is null".to_string());
            }
            Ok(&*vtbl_ptr)
        }
    }

    fn set_trace_level(&self, vtbl: &ISVCEncoderVtbl) -> Result<(), String> {
        let set_opt = vtbl.SetOption.ok_or("SetOption not in vtable")?;
        let mut level: c_int = if self.config.debug_log {
            WELS_LOG_WARNING
        } else {
            WELS_LOG_QUIET
        };
        let ret = unsafe {
            set_opt(
                self.encoder_ptr,
                ENCODER_OPTION_TRACE_LEVEL,
                addr_of_mut!(level) as *mut c_void,
            )
        };
        if ret != 0 {
            warn!("Failed to set trace level: {ret}");
        }
        Ok(())
    }

    fn set_data_format(&self, vtbl: &ISVCEncoderVtbl) -> Result<(), String> {
        let set_opt = vtbl.SetOption.ok_or("SetOption not in vtable")?;
        let mut format: c_int = VIDEO_FORMAT_I420;
        let ret = unsafe {
            set_opt(
                self.encoder_ptr,
                ENCODER_OPTION_DATAFORMAT,
                addr_of_mut!(format) as *mut c_void,
            )
        };
        if ret != 0 {
            warn!("Failed to set data format: {ret}");
        }
        Ok(())
    }

    pub(crate) fn abi(&self) -> AbiGeneration {
        self.abi
    }
}

impl Drop for VersionedEncoder {
    fn drop(&mut self) {
        if !self.encoder_ptr.is_null() {
            // Uninitialize first, then destroy
            if let Ok(vtbl) = self.vtable() {
                if let Some(uninit) = vtbl.Uninitialize {
                    unsafe { uninit(self.encoder_ptr) };
                }
            }
            unsafe { self.api.destroy_encoder_instance(self.encoder_ptr) };
        }
    }
}
