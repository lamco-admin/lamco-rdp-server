#![expect(
    unsafe_code,
    reason = "FFI struct defaults via mem::zeroed and raw pointer extraction"
)]
// Field names match the OpenH264 C API exactly — renaming would break FFI layout.
// Dead code allowed: complete API binding — constants/fields used on demand.
#![allow(non_snake_case, non_camel_case_types, dead_code)]

//! Version-specific FFI struct definitions for OpenH264.
//!
//! OpenH264 2.6.0 added PSNR fields to several structs, bumping ABI from 7 to 8.
//! This module defines both layouts so the encoder can work with either version.
//!
//! The vtable (`ISVCEncoderVtbl`) and enum types are identical across all versions
//! we support (2.3.1+). Only the data structs passed through vtable function
//! pointers differ between ABI 7 and ABI 8.

use std::os::raw::{c_int, c_longlong, c_uchar, c_uint, c_ushort, c_void};

// ============================================================================
// Version-stable types (identical across ABI 7 and ABI 8)
// ============================================================================

pub(crate) type EVideoFormatType = c_int;
pub(crate) type EVideoFrameType = c_int;
pub(crate) type ENCODER_OPTION = c_int;
pub(crate) type EUsageType = c_int;
pub(crate) type ECOMPLEXITY_MODE = c_int;
pub(crate) type RC_MODES = c_int;
pub(crate) type EParameterSetStrategy = c_int;
pub(crate) type EProfileIdc = c_int;
pub(crate) type ELevelIdc = c_int;
pub(crate) type ESampleAspectRatio = c_int;
pub(crate) type SliceModeEnum = c_int;

// Video format constants
pub(crate) const VIDEO_FORMAT_I420: EVideoFormatType = 23;

// Frame type constants
pub(crate) const VIDEO_FRAME_TYPE_INVALID: EVideoFrameType = 0;
pub(crate) const VIDEO_FRAME_TYPE_IDR: EVideoFrameType = 1;
pub(crate) const VIDEO_FRAME_TYPE_I: EVideoFrameType = 2;
pub(crate) const VIDEO_FRAME_TYPE_P: EVideoFrameType = 3;
pub(crate) const VIDEO_FRAME_TYPE_SKIP: EVideoFrameType = 4;

// Encoder option constants
pub(crate) const ENCODER_OPTION_DATAFORMAT: ENCODER_OPTION = 0;
pub(crate) const ENCODER_OPTION_SVC_ENCODE_PARAM_EXT: ENCODER_OPTION = 3;
pub(crate) const ENCODER_OPTION_TRACE_LEVEL: ENCODER_OPTION = 30;

// Usage type constants
pub(crate) const SCREEN_CONTENT_REAL_TIME: EUsageType = 1;

// Complexity constants
pub(crate) const LOW_COMPLEXITY: ECOMPLEXITY_MODE = 0;
pub(crate) const MEDIUM_COMPLEXITY: ECOMPLEXITY_MODE = 1;
pub(crate) const HIGH_COMPLEXITY: ECOMPLEXITY_MODE = 2;

// Slice mode constants
pub(crate) const SM_SINGLE_SLICE: SliceModeEnum = 0;
pub(crate) const SM_SIZELIMITED_SLICE: SliceModeEnum = 3;

// Deblocking constants
pub(crate) const DEBLOCKING_IDC_0: c_int = 0;

// Log level constants
pub(crate) const WELS_LOG_QUIET: c_int = 0;
pub(crate) const WELS_LOG_WARNING: c_int = 2;

// RC mode constants
pub(crate) const RC_QUALITY_MODE: RC_MODES = 0;
pub(crate) const RC_BITRATE_MODE: RC_MODES = 1;

// SPS/PPS ID strategy constants
pub(crate) const CONSTANT_ID: EParameterSetStrategy = 1;

/// OpenH264 version struct. Stable across all versions.
#[repr(C)]
#[derive(Debug, Copy, Clone, Default)]
pub(crate) struct OpenH264Version {
    pub uMajor: c_uint,
    pub uMinor: c_uint,
    pub uRevision: c_uint,
    pub uReserved: c_uint,
}

/// Slice argument configuration. Stable across all versions we support.
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub(crate) struct SSliceArgument {
    pub uiSliceMode: SliceModeEnum,
    pub uiSliceNum: c_uint,
    pub uiSliceMbNum: [c_uint; 35],
    pub uiSliceSizeConstraint: c_uint,
}

impl Default for SSliceArgument {
    fn default() -> Self {
        unsafe { std::mem::zeroed() }
    }
}

/// Spatial layer configuration. Stable across all versions we support.
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub(crate) struct SSpatialLayerConfig {
    pub iVideoWidth: c_int,
    pub iVideoHeight: c_int,
    pub fFrameRate: f32,
    pub iSpatialBitrate: c_int,
    pub iMaxSpatialBitrate: c_int,
    pub uiProfileIdc: EProfileIdc,
    pub uiLevelIdc: ELevelIdc,
    pub iDLayerQp: c_int,
    pub sSliceArgument: SSliceArgument,
    pub bVideoSignalTypePresent: bool,
    pub uiVideoFormat: c_uchar,
    pub bFullRange: bool,
    pub bColorDescriptionPresent: bool,
    pub uiColorPrimaries: c_uchar,
    pub uiTransferCharacteristics: c_uchar,
    pub uiColorMatrix: c_uchar,
    pub bAspectRatioPresent: bool,
    pub eAspectRatio: ESampleAspectRatio,
    pub sAspectRatioExtWidth: c_ushort,
    pub sAspectRatioExtHeight: c_ushort,
}

impl Default for SSpatialLayerConfig {
    fn default() -> Self {
        unsafe { std::mem::zeroed() }
    }
}

// ============================================================================
// The ISVCEncoder vtable. Identical across all versions.
//
// Function pointer signatures use *const c_void for struct parameters that
// differ between ABI versions. The caller casts to the correct type.
// ============================================================================

/// Opaque encoder handle. Points to a vtable pointer.
pub(crate) type ISVCEncoder = *const ISVCEncoderVtbl;

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub(crate) struct ISVCEncoderVtbl {
    pub Initialize:
        Option<unsafe extern "C" fn(encoder: *mut ISVCEncoder, pParam: *const c_void) -> c_int>,
    pub InitializeExt:
        Option<unsafe extern "C" fn(encoder: *mut ISVCEncoder, pParam: *const c_void) -> c_int>,
    pub GetDefaultParams:
        Option<unsafe extern "C" fn(encoder: *mut ISVCEncoder, pParam: *mut c_void) -> c_int>,
    pub Uninitialize: Option<unsafe extern "C" fn(encoder: *mut ISVCEncoder) -> c_int>,
    pub EncodeFrame: Option<
        unsafe extern "C" fn(
            encoder: *mut ISVCEncoder,
            kpSrcPic: *const c_void,
            pBsInfo: *mut c_void,
        ) -> c_int,
    >,
    pub EncodeParameterSets:
        Option<unsafe extern "C" fn(encoder: *mut ISVCEncoder, pBsInfo: *mut c_void) -> c_int>,
    pub ForceIntraFrame:
        Option<unsafe extern "C" fn(encoder: *mut ISVCEncoder, bIDR: bool) -> c_int>,
    pub SetOption: Option<
        unsafe extern "C" fn(
            encoder: *mut ISVCEncoder,
            eOptionId: ENCODER_OPTION,
            pOption: *mut c_void,
        ) -> c_int,
    >,
    pub GetOption: Option<
        unsafe extern "C" fn(
            encoder: *mut ISVCEncoder,
            eOptionId: ENCODER_OPTION,
            pOption: *mut c_void,
        ) -> c_int,
    >,
}

impl Default for ISVCEncoderVtbl {
    fn default() -> Self {
        unsafe { std::mem::zeroed() }
    }
}

// ============================================================================
// ABI 7 structs (OpenH264 2.3.1 through 2.5.x)
// ============================================================================

pub(crate) mod abi7 {
    use super::{
        c_int, c_longlong, c_uchar, c_uint, c_ushort, EParameterSetStrategy, EUsageType,
        EVideoFrameType, SSpatialLayerConfig, ECOMPLEXITY_MODE, RC_MODES,
    };

    #[repr(C)]
    #[derive(Debug, Copy, Clone)]
    pub(crate) struct SLayerBSInfo {
        pub uiTemporalId: c_uchar,
        pub uiSpatialId: c_uchar,
        pub uiQualityId: c_uchar,
        pub eFrameType: EVideoFrameType,
        pub uiLayerType: c_uchar,
        pub iSubSeqId: c_int,
        pub iNalCount: c_int,
        pub pNalLengthInByte: *mut c_int,
        pub pBsBuf: *mut c_uchar,
    }

    impl Default for SLayerBSInfo {
        fn default() -> Self {
            unsafe { std::mem::zeroed() }
        }
    }

    #[repr(C)]
    #[derive(Clone)]
    pub(crate) struct SFrameBSInfo {
        pub iLayerNum: c_int,
        pub sLayerInfo: [SLayerBSInfo; 128],
        pub eFrameType: EVideoFrameType,
        pub iFrameSizeInBytes: c_int,
        pub uiTimeStamp: c_longlong,
    }

    impl Default for SFrameBSInfo {
        fn default() -> Self {
            unsafe { std::mem::zeroed() }
        }
    }

    #[repr(C)]
    #[derive(Debug, Copy, Clone)]
    pub(crate) struct SSourcePicture {
        pub iColorFormat: c_int,
        pub iStride: [c_int; 4],
        pub pData: [*mut c_uchar; 4],
        pub iPicWidth: c_int,
        pub iPicHeight: c_int,
        pub uiTimeStamp: c_longlong,
    }

    impl Default for SSourcePicture {
        fn default() -> Self {
            unsafe { std::mem::zeroed() }
        }
    }

    /// SEncParamExt for ABI 7 (no trailing PSNR fields)
    #[repr(C)]
    #[derive(Clone)]
    pub(crate) struct SEncParamExt {
        pub iUsageType: EUsageType,
        pub iPicWidth: c_int,
        pub iPicHeight: c_int,
        pub iTargetBitrate: c_int,
        pub iRCMode: RC_MODES,
        pub fMaxFrameRate: f32,
        pub iTemporalLayerNum: c_int,
        pub iSpatialLayerNum: c_int,
        pub sSpatialLayers: [SSpatialLayerConfig; 4],
        pub iComplexityMode: ECOMPLEXITY_MODE,
        pub uiIntraPeriod: c_uint,
        pub iNumRefFrame: c_int,
        pub eSpsPpsIdStrategy: EParameterSetStrategy,
        pub bPrefixNalAddingCtrl: bool,
        pub bEnableSSEI: bool,
        pub bSimulcastAVC: bool,
        pub iPaddingFlag: c_int,
        pub iEntropyCodingModeFlag: c_int,
        pub bEnableFrameSkip: bool,
        pub iMaxBitrate: c_int,
        pub iMaxQp: c_int,
        pub iMinQp: c_int,
        pub uiMaxNalSize: c_uint,
        pub bEnableLongTermReference: bool,
        pub iLTRRefNum: c_int,
        pub iLtrMarkPeriod: c_uint,
        pub iMultipleThreadIdc: c_ushort,
        pub bUseLoadBalancing: bool,
        pub iLoopFilterDisableIdc: c_int,
        pub iLoopFilterAlphaC0Offset: c_int,
        pub iLoopFilterBetaOffset: c_int,
        pub bEnableDenoise: bool,
        pub bEnableBackgroundDetection: bool,
        pub bEnableAdaptiveQuant: bool,
        pub bEnableFrameCroppingFlag: bool,
        pub bEnableSceneChangeDetect: bool,
        pub bIsLosslessLink: bool,
        pub bFixRCOverShoot: bool,
        pub iIdrBitrateRatio: c_int,
    }

    impl Default for SEncParamExt {
        fn default() -> Self {
            unsafe { std::mem::zeroed() }
        }
    }
}

// ============================================================================
// ABI 8 structs (OpenH264 2.6.0+)
// ============================================================================

pub(crate) mod abi8 {
    use super::{
        c_int, c_longlong, c_uchar, c_uint, c_ushort, EParameterSetStrategy, EUsageType,
        EVideoFrameType, SSpatialLayerConfig, ECOMPLEXITY_MODE, RC_MODES,
    };

    #[repr(C)]
    #[derive(Debug, Copy, Clone)]
    pub(crate) struct SLayerBSInfo {
        pub uiTemporalId: c_uchar,
        pub uiSpatialId: c_uchar,
        pub uiQualityId: c_uchar,
        pub eFrameType: EVideoFrameType,
        pub uiLayerType: c_uchar,
        pub iSubSeqId: c_int,
        pub iNalCount: c_int,
        pub pNalLengthInByte: *mut c_int,
        pub pBsBuf: *mut c_uchar,
        pub rPsnr: [f32; 3], // NEW in 2.6.0
    }

    impl Default for SLayerBSInfo {
        fn default() -> Self {
            unsafe { std::mem::zeroed() }
        }
    }

    #[repr(C)]
    #[derive(Clone)]
    pub(crate) struct SFrameBSInfo {
        pub iLayerNum: c_int,
        pub sLayerInfo: [SLayerBSInfo; 128],
        pub eFrameType: EVideoFrameType,
        pub iFrameSizeInBytes: c_int,
        pub uiTimeStamp: c_longlong,
    }

    impl Default for SFrameBSInfo {
        fn default() -> Self {
            unsafe { std::mem::zeroed() }
        }
    }

    #[repr(C)]
    #[derive(Debug, Copy, Clone)]
    pub(crate) struct SSourcePicture {
        pub iColorFormat: c_int,
        pub iStride: [c_int; 4],
        pub pData: [*mut c_uchar; 4],
        pub iPicWidth: c_int,
        pub iPicHeight: c_int,
        pub uiTimeStamp: c_longlong,
        pub bPsnrY: bool, // NEW in 2.6.0
        pub bPsnrU: bool,
        pub bPsnrV: bool,
    }

    impl Default for SSourcePicture {
        fn default() -> Self {
            unsafe { std::mem::zeroed() }
        }
    }

    /// SEncParamExt for ABI 8 (with trailing PSNR fields)
    #[repr(C)]
    #[derive(Clone)]
    pub(crate) struct SEncParamExt {
        pub iUsageType: EUsageType,
        pub iPicWidth: c_int,
        pub iPicHeight: c_int,
        pub iTargetBitrate: c_int,
        pub iRCMode: RC_MODES,
        pub fMaxFrameRate: f32,
        pub iTemporalLayerNum: c_int,
        pub iSpatialLayerNum: c_int,
        pub sSpatialLayers: [SSpatialLayerConfig; 4],
        pub iComplexityMode: ECOMPLEXITY_MODE,
        pub uiIntraPeriod: c_uint,
        pub iNumRefFrame: c_int,
        pub eSpsPpsIdStrategy: EParameterSetStrategy,
        pub bPrefixNalAddingCtrl: bool,
        pub bEnableSSEI: bool,
        pub bSimulcastAVC: bool,
        pub iPaddingFlag: c_int,
        pub iEntropyCodingModeFlag: c_int,
        pub bEnableFrameSkip: bool,
        pub iMaxBitrate: c_int,
        pub iMaxQp: c_int,
        pub iMinQp: c_int,
        pub uiMaxNalSize: c_uint,
        pub bEnableLongTermReference: bool,
        pub iLTRRefNum: c_int,
        pub iLtrMarkPeriod: c_uint,
        pub iMultipleThreadIdc: c_ushort,
        pub bUseLoadBalancing: bool,
        pub iLoopFilterDisableIdc: c_int,
        pub iLoopFilterAlphaC0Offset: c_int,
        pub iLoopFilterBetaOffset: c_int,
        pub bEnableDenoise: bool,
        pub bEnableBackgroundDetection: bool,
        pub bEnableAdaptiveQuant: bool,
        pub bEnableFrameCroppingFlag: bool,
        pub bEnableSceneChangeDetect: bool,
        pub bIsLosslessLink: bool,
        pub bFixRCOverShoot: bool,
        pub iIdrBitrateRatio: c_int,
        pub bPsnrY: bool, // NEW in 2.6.0
        pub bPsnrU: bool,
        pub bPsnrV: bool,
    }

    impl Default for SEncParamExt {
        fn default() -> Self {
            unsafe { std::mem::zeroed() }
        }
    }
}

// ============================================================================
// Trait for extracting encoded data from version-specific SFrameBSInfo
// ============================================================================

/// Common encoded frame data extracted from either ABI version.
pub(crate) struct EncodedFrameData {
    pub frame_type: EVideoFrameType,
    pub frame_size: usize,
    pub layers: Vec<LayerData>,
}

pub(crate) struct LayerData {
    pub temporal_id: u8,
    pub spatial_id: u8,
    pub frame_type: EVideoFrameType,
    pub nal_data: Vec<u8>,
}

impl EncodedFrameData {
    /// Extract from ABI 7 SFrameBSInfo
    pub(crate) unsafe fn from_abi7(info: &abi7::SFrameBSInfo) -> Self {
        let mut layers = Vec::new();
        for i in 0..info.iLayerNum as usize {
            let layer = &info.sLayerInfo[i];
            if layer.iNalCount <= 0 || layer.pBsBuf.is_null() {
                continue;
            }
            let total_len: usize = (0..layer.iNalCount as usize)
                .map(|j| unsafe { *layer.pNalLengthInByte.add(j) } as usize)
                .sum();
            let data = unsafe { std::slice::from_raw_parts(layer.pBsBuf, total_len) }.to_vec();
            layers.push(LayerData {
                temporal_id: layer.uiTemporalId,
                spatial_id: layer.uiSpatialId,
                frame_type: layer.eFrameType,
                nal_data: data,
            });
        }
        Self {
            frame_type: info.eFrameType,
            frame_size: info.iFrameSizeInBytes as usize,
            layers,
        }
    }

    /// Extract from ABI 8 SFrameBSInfo
    pub(crate) unsafe fn from_abi8(info: &abi8::SFrameBSInfo) -> Self {
        let mut layers = Vec::new();
        for i in 0..info.iLayerNum as usize {
            let layer = &info.sLayerInfo[i];
            if layer.iNalCount <= 0 || layer.pBsBuf.is_null() {
                continue;
            }
            let total_len: usize = (0..layer.iNalCount as usize)
                .map(|j| unsafe { *layer.pNalLengthInByte.add(j) } as usize)
                .sum();
            let data = unsafe { std::slice::from_raw_parts(layer.pBsBuf, total_len) }.to_vec();
            layers.push(LayerData {
                temporal_id: layer.uiTemporalId,
                spatial_id: layer.uiSpatialId,
                frame_type: layer.eFrameType,
                nal_data: data,
            });
        }
        Self {
            frame_type: info.eFrameType,
            frame_size: info.iFrameSizeInBytes as usize,
            layers,
        }
    }

    /// Collect all NAL data into a single Annex B byte stream.
    pub(crate) fn to_vec(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.frame_size);
        for layer in &self.layers {
            out.extend_from_slice(&layer.nal_data);
        }
        out
    }

    pub(crate) fn is_keyframe(&self) -> bool {
        self.frame_type == VIDEO_FRAME_TYPE_IDR || self.frame_type == VIDEO_FRAME_TYPE_I
    }
}
