//! CoreMedia CMSampleBuffer and CMBlockBuffer FFI bindings.
//!
//! These functions allow extraction of raw data from encoded video frames,
//! which is necessary for streaming scenarios where AVAssetWriter cannot be used.
//!
//! VideoToolbox encodes video to CMSampleBuffer objects containing H.264/HEVC NAL units.
//! This module provides functions to:
//! - Access the raw encoded data (CMBlockBuffer)
//! - Extract H.264 parameter sets (SPS/PPS) from format descriptions
//! - Get timing information (PTS, DTS, duration)
//! - Check sample attachment properties (sync samples/keyframes)

use core_foundation_sys::base::OSStatus;
use core_media_sys::{CMFormatDescriptionRef, CMSampleBufferRef, CMTime};
use libc::c_void;

/// Opaque type for CMBlockBuffer.
#[repr(C)]
pub struct __CMBlockBuffer {
    _private: c_void,
}

/// Reference to a CoreMedia block buffer containing raw encoded data.
pub type CMBlockBufferRef = *mut __CMBlockBuffer;

// CMSampleBuffer attachment keys
#[link(name = "CoreMedia", kind = "framework")]
extern "C" {
    /// Key to check if a sample buffer is a sync sample (keyframe).
    /// Value is a CFBoolean.
    pub static kCMSampleAttachmentKey_NotSync: *const c_void;

    /// Key to check if a sample depends on other samples.
    /// Value is a CFBoolean.
    pub static kCMSampleAttachmentKey_DependsOnOthers: *const c_void;
}

#[link(name = "CoreMedia", kind = "framework")]
extern "C" {
    // ============================================
    // CMSampleBuffer access functions
    // ============================================

    /// Returns the CMBlockBuffer containing the media data for the sample buffer.
    ///
    /// Returns NULL if the sample buffer has no data buffer (e.g., for gap samples).
    pub fn CMSampleBufferGetDataBuffer(sbuf: CMSampleBufferRef) -> CMBlockBufferRef;

    /// Returns the format description of the samples in the buffer.
    ///
    /// For video, this contains codec information including H.264 parameter sets.
    pub fn CMSampleBufferGetFormatDescription(sbuf: CMSampleBufferRef) -> CMFormatDescriptionRef;

    /// Returns the presentation timestamp of the first sample.
    pub fn CMSampleBufferGetPresentationTimeStamp(sbuf: CMSampleBufferRef) -> CMTime;

    /// Returns the decode timestamp of the first sample.
    ///
    /// For B-frames, DTS differs from PTS. Returns kCMTimeInvalid if not available.
    pub fn CMSampleBufferGetDecodeTimeStamp(sbuf: CMSampleBufferRef) -> CMTime;

    /// Returns the duration of the sample buffer.
    pub fn CMSampleBufferGetDuration(sbuf: CMSampleBufferRef) -> CMTime;

    /// Returns the total size of all sample data in the buffer.
    pub fn CMSampleBufferGetTotalSampleSize(sbuf: CMSampleBufferRef) -> usize;

    /// Returns the number of samples in the buffer.
    ///
    /// For video, this is typically 1. For audio, it can be many samples.
    pub fn CMSampleBufferGetNumSamples(sbuf: CMSampleBufferRef) -> i64;

    /// Returns an array of sample attachments dictionaries.
    ///
    /// The `createIfNecessary` parameter controls whether to create empty attachments.
    /// Use this to check if a sample is a sync sample (keyframe).
    pub fn CMSampleBufferGetSampleAttachmentsArray(
        sbuf: CMSampleBufferRef,
        createIfNecessary: u8,
    ) -> *const c_void; // CFArrayRef

    // ============================================
    // CMBlockBuffer access functions
    // ============================================

    /// Returns the total data length of the block buffer.
    pub fn CMBlockBufferGetDataLength(theBuffer: CMBlockBufferRef) -> usize;

    /// Returns a pointer to the contiguous data in the block buffer.
    ///
    /// # Arguments
    /// * `theBuffer` - The block buffer
    /// * `offset` - Byte offset into the data
    /// * `lengthAtOffsetOut` - Returns the contiguous length available at offset
    /// * `totalLengthOut` - Returns the total length of the buffer
    /// * `dataPointerOut` - Returns a pointer to the data
    ///
    /// # Safety
    /// The returned pointer is only valid while the block buffer is alive.
    pub fn CMBlockBufferGetDataPointer(
        theBuffer: CMBlockBufferRef,
        offset: usize,
        lengthAtOffsetOut: *mut usize,
        totalLengthOut: *mut usize,
        dataPointerOut: *mut *mut u8,
    ) -> OSStatus;

    /// Copies data from a block buffer into a destination buffer.
    ///
    /// Use this if the block buffer data is not contiguous.
    pub fn CMBlockBufferCopyDataBytes(
        theSourceBuffer: CMBlockBufferRef,
        offsetToData: usize,
        dataLength: usize,
        destination: *mut c_void,
    ) -> OSStatus;

    /// Checks if the block buffer's data is contiguous in memory.
    pub fn CMBlockBufferIsRangeContiguous(
        theBuffer: CMBlockBufferRef,
        offset: usize,
        length: usize,
    ) -> u8;

    // ============================================
    // H.264/HEVC parameter set extraction
    // ============================================

    /// Gets H.264 parameter sets (SPS/PPS) from a video format description.
    ///
    /// # Arguments
    /// * `videoDesc` - The video format description from CMSampleBufferGetFormatDescription
    /// * `parameterSetIndex` - 0 for SPS, 1 for PPS (may have more for HEVC)
    /// * `parameterSetPointerOut` - Returns a pointer to the parameter set data
    /// * `parameterSetSizeOut` - Returns the size of the parameter set
    /// * `parameterSetCountOut` - Returns the total number of parameter sets
    /// * `NALUnitHeaderLengthOut` - Returns the NAL unit length field size (typically 4)
    ///
    /// # Returns
    /// - noErr (0) on success
    /// - kCMFormatDescriptionError_InvalidParameter if videoDesc is not H.264
    ///
    /// # Safety
    /// The returned pointer is only valid while the format description is alive.
    pub fn CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
        videoDesc: CMFormatDescriptionRef,
        parameterSetIndex: usize,
        parameterSetPointerOut: *mut *const u8,
        parameterSetSizeOut: *mut usize,
        parameterSetCountOut: *mut usize,
        NALUnitHeaderLengthOut: *mut i32,
    ) -> OSStatus;

    /// Gets HEVC (H.265) parameter sets (VPS/SPS/PPS) from a video format description.
    ///
    /// Similar to the H.264 version but for HEVC content.
    /// Index 0 = VPS, 1 = SPS, 2 = PPS (may have more).
    pub fn CMVideoFormatDescriptionGetHEVCParameterSetAtIndex(
        videoDesc: CMFormatDescriptionRef,
        parameterSetIndex: usize,
        parameterSetPointerOut: *mut *const u8,
        parameterSetSizeOut: *mut usize,
        parameterSetCountOut: *mut usize,
        NALUnitHeaderLengthOut: *mut i32,
    ) -> OSStatus;

    // ============================================
    // Video format description utilities
    // ============================================

    /// Returns the dimensions of the video format description.
    pub fn CMVideoFormatDescriptionGetDimensions(
        videoDesc: CMFormatDescriptionRef,
    ) -> CMVideoDimensions;

    /// Returns the codec type (FourCC) of the format description.
    pub fn CMFormatDescriptionGetMediaSubType(desc: CMFormatDescriptionRef) -> u32;
}

/// Video dimensions structure.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct CMVideoDimensions {
    pub width: i32,
    pub height: i32,
}

/// H.264 NAL unit types.
pub mod nal_unit_type {
    /// Non-IDR slice (P/B frame)
    pub const NON_IDR_SLICE: u8 = 1;
    /// Coded slice data partition A
    pub const SLICE_DATA_A: u8 = 2;
    /// Coded slice data partition B
    pub const SLICE_DATA_B: u8 = 3;
    /// Coded slice data partition C
    pub const SLICE_DATA_C: u8 = 4;
    /// IDR slice (keyframe)
    pub const IDR_SLICE: u8 = 5;
    /// Supplemental enhancement information
    pub const SEI: u8 = 6;
    /// Sequence parameter set
    pub const SPS: u8 = 7;
    /// Picture parameter set
    pub const PPS: u8 = 8;
    /// Access unit delimiter
    pub const AUD: u8 = 9;
    /// End of sequence
    pub const END_OF_SEQ: u8 = 10;
    /// End of stream
    pub const END_OF_STREAM: u8 = 11;
    /// Filler data
    pub const FILLER: u8 = 12;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_video_dimensions_default() {
        let dims = CMVideoDimensions::default();
        assert_eq!(dims.width, 0);
        assert_eq!(dims.height, 0);
    }
}
