//! NAL unit extraction from CMSampleBuffer.
//!
//! VideoToolbox outputs H.264 encoded data in AVCC format (length-prefixed NAL units).
//! This module extracts NAL units for use with fMP4 muxing or Annex B conversion.
//!
//! # AVCC Format
//!
//! NAL units are stored as:
//! ```text
//! [4-byte big-endian length][NAL data][4-byte length][NAL data]...
//! ```
//!
//! # NAL Unit Types (H.264)
//!
//! - Type 1: Non-IDR slice (P/B frame)
//! - Type 5: IDR slice (keyframe)
//! - Type 7: SPS (Sequence Parameter Set)
//! - Type 8: PPS (Picture Parameter Set)
//!
//! # Example
//!
//! ```no_run
//! use video_toolbox_sys::helpers::nal_extractor::{NalExtractor, NalUnit};
//!
//! // In compression callback, extract NAL units:
//! // let extractor = NalExtractor::new();
//! // let (sps, pps) = extractor.extract_parameter_sets(format_desc)?;
//! // let nals = extractor.extract_nal_units(sample_buffer)?;
//! ```

use crate::cm_sample_buffer::{
    nal_unit_type, CMBlockBufferGetDataLength, CMBlockBufferGetDataPointer,
    CMSampleBufferGetDataBuffer, CMSampleBufferGetDecodeTimeStamp, CMSampleBufferGetDuration,
    CMSampleBufferGetFormatDescription, CMSampleBufferGetPresentationTimeStamp,
    CMSampleBufferGetSampleAttachmentsArray, CMVideoFormatDescriptionGetDimensions,
    CMVideoFormatDescriptionGetH264ParameterSetAtIndex, kCMSampleAttachmentKey_NotSync,
};
use core_foundation_sys::array::CFArrayGetValueAtIndex;
use core_foundation_sys::base::CFTypeRef;
use core_foundation_sys::dictionary::CFDictionaryGetValue;
use core_media_sys::{CMFormatDescriptionRef, CMSampleBufferRef, CMTime};
use std::ptr;

/// Error codes for NAL extraction operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NalError {
    /// Sample buffer has no data buffer
    NoDataBuffer,
    /// Failed to get data pointer from block buffer
    DataPointerFailed(i32),
    /// Format description is null
    NoFormatDescription,
    /// Failed to get parameter set
    ParameterSetFailed(i32),
    /// Invalid NAL unit length
    InvalidNalLength,
    /// Buffer too small for NAL data
    BufferTooSmall,
}

impl std::fmt::Display for NalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NalError::NoDataBuffer => write!(f, "Sample buffer has no data buffer"),
            NalError::DataPointerFailed(code) => {
                write!(f, "Failed to get data pointer: OSStatus {}", code)
            }
            NalError::NoFormatDescription => write!(f, "Format description is null"),
            NalError::ParameterSetFailed(code) => {
                write!(f, "Failed to get parameter set: OSStatus {}", code)
            }
            NalError::InvalidNalLength => write!(f, "Invalid NAL unit length"),
            NalError::BufferTooSmall => write!(f, "Buffer too small for NAL data"),
        }
    }
}

impl std::error::Error for NalError {}

/// A single H.264 NAL unit.
#[derive(Debug, Clone)]
pub struct NalUnit {
    /// The raw NAL unit data (without length prefix, without start code).
    pub data: Vec<u8>,
    /// NAL unit type (from first byte & 0x1F).
    pub nal_type: u8,
}

impl NalUnit {
    /// Returns true if this NAL unit is an IDR (keyframe) slice.
    pub fn is_idr(&self) -> bool {
        self.nal_type == nal_unit_type::IDR_SLICE
    }

    /// Returns true if this NAL unit is an SPS.
    pub fn is_sps(&self) -> bool {
        self.nal_type == nal_unit_type::SPS
    }

    /// Returns true if this NAL unit is a PPS.
    pub fn is_pps(&self) -> bool {
        self.nal_type == nal_unit_type::PPS
    }

    /// Returns true if this NAL unit is a video slice (IDR or non-IDR).
    pub fn is_slice(&self) -> bool {
        self.nal_type == nal_unit_type::IDR_SLICE || self.nal_type == nal_unit_type::NON_IDR_SLICE
    }

    /// Convert NAL unit to Annex B format (with 0x00000001 start code).
    pub fn to_annex_b(&self) -> Vec<u8> {
        let mut result = Vec::with_capacity(4 + self.data.len());
        result.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
        result.extend_from_slice(&self.data);
        result
    }
}

/// Timing information extracted from a sample buffer.
#[derive(Debug, Clone, Copy)]
pub struct SampleTiming {
    /// Presentation timestamp in timescale units.
    pub pts: i64,
    /// Decode timestamp in timescale units (same as PTS if no B-frames).
    pub dts: i64,
    /// Duration in timescale units.
    pub duration: i64,
    /// The timescale (e.g., 90000 for standard video).
    pub timescale: i32,
}

impl SampleTiming {
    /// Convert PTS to seconds.
    pub fn pts_seconds(&self) -> f64 {
        self.pts as f64 / self.timescale as f64
    }

    /// Convert DTS to seconds.
    pub fn dts_seconds(&self) -> f64 {
        self.dts as f64 / self.timescale as f64
    }

    /// Convert duration to seconds.
    pub fn duration_seconds(&self) -> f64 {
        self.duration as f64 / self.timescale as f64
    }
}

/// H.264 parameter sets (SPS and PPS) extracted from format description.
#[derive(Debug, Clone)]
pub struct H264ParameterSets {
    /// Sequence Parameter Set (defines video dimensions, profile, level, etc.)
    pub sps: Vec<u8>,
    /// Picture Parameter Set (defines encoding parameters)
    pub pps: Vec<u8>,
    /// NAL unit length field size (typically 4 bytes).
    pub nal_length_size: i32,
}

/// Video dimensions.
#[derive(Debug, Clone, Copy)]
pub struct VideoDimensions {
    pub width: u32,
    pub height: u32,
}

/// NAL unit extractor for CMSampleBuffer.
///
/// This struct provides methods to extract H.264 NAL units from VideoToolbox
/// encoded sample buffers. The data can then be used for fMP4 muxing or
/// other streaming purposes.
pub struct NalExtractor {
    _private: (),
}

impl Default for NalExtractor {
    fn default() -> Self {
        Self::new()
    }
}

impl NalExtractor {
    /// Create a new NAL extractor.
    pub fn new() -> Self {
        Self { _private: () }
    }

    /// Extract H.264 parameter sets (SPS and PPS) from a format description.
    ///
    /// This should be called once when the first encoded frame is received,
    /// as the parameter sets are needed for the fMP4 initialization segment.
    ///
    /// # Safety
    ///
    /// The format description must be a valid H.264 video format description.
    pub unsafe fn extract_parameter_sets(
        &self,
        format_desc: CMFormatDescriptionRef,
    ) -> Result<H264ParameterSets, NalError> {
        if format_desc.is_null() {
            return Err(NalError::NoFormatDescription);
        }

        let mut sps_ptr: *const u8 = ptr::null();
        let mut sps_size: usize = 0;
        let mut param_count: usize = 0;
        let mut nal_length_size: i32 = 0;

        // Get SPS (index 0)
        let status = CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
            format_desc,
            0, // SPS index
            &mut sps_ptr,
            &mut sps_size,
            &mut param_count,
            &mut nal_length_size,
        );

        if status != 0 {
            return Err(NalError::ParameterSetFailed(status));
        }

        let sps = std::slice::from_raw_parts(sps_ptr, sps_size).to_vec();

        // Get PPS (index 1)
        let mut pps_ptr: *const u8 = ptr::null();
        let mut pps_size: usize = 0;

        let status = CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
            format_desc,
            1, // PPS index
            &mut pps_ptr,
            &mut pps_size,
            ptr::null_mut(),
            ptr::null_mut(),
        );

        if status != 0 {
            return Err(NalError::ParameterSetFailed(status));
        }

        let pps = std::slice::from_raw_parts(pps_ptr, pps_size).to_vec();

        Ok(H264ParameterSets {
            sps,
            pps,
            nal_length_size,
        })
    }

    /// Extract video dimensions from a format description.
    ///
    /// # Safety
    ///
    /// The format description must be a valid video format description.
    pub unsafe fn get_dimensions(
        &self,
        format_desc: CMFormatDescriptionRef,
    ) -> Result<VideoDimensions, NalError> {
        if format_desc.is_null() {
            return Err(NalError::NoFormatDescription);
        }

        let dims = CMVideoFormatDescriptionGetDimensions(format_desc);
        Ok(VideoDimensions {
            width: dims.width as u32,
            height: dims.height as u32,
        })
    }

    /// Extract NAL units from an encoded sample buffer.
    ///
    /// VideoToolbox encodes H.264 in AVCC format where each NAL unit is
    /// prefixed with its length (typically 4 bytes, big-endian).
    ///
    /// # Safety
    ///
    /// The sample buffer must be a valid encoded H.264 sample buffer.
    pub unsafe fn extract_nal_units(
        &self,
        sample_buffer: CMSampleBufferRef,
    ) -> Result<Vec<NalUnit>, NalError> {
        let block_buffer = CMSampleBufferGetDataBuffer(sample_buffer);
        if block_buffer.is_null() {
            return Err(NalError::NoDataBuffer);
        }

        let total_length = CMBlockBufferGetDataLength(block_buffer);
        if total_length == 0 {
            return Ok(Vec::new());
        }

        let mut data_ptr: *mut u8 = ptr::null_mut();
        let mut length_at_offset: usize = 0;
        let mut total_len_out: usize = 0;

        let status = CMBlockBufferGetDataPointer(
            block_buffer,
            0,
            &mut length_at_offset,
            &mut total_len_out,
            &mut data_ptr,
        );

        if status != 0 {
            return Err(NalError::DataPointerFailed(status));
        }

        // Get NAL unit length size from format description
        let format_desc = CMSampleBufferGetFormatDescription(sample_buffer);
        let nal_length_size = if !format_desc.is_null() {
            let mut length_size: i32 = 4;
            CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
                format_desc,
                0,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                &mut length_size,
            );
            length_size as usize
        } else {
            4 // Default to 4 bytes
        };

        self.parse_avcc_data(data_ptr, total_length, nal_length_size)
    }

    /// Parse AVCC format data into NAL units.
    unsafe fn parse_avcc_data(
        &self,
        data_ptr: *const u8,
        total_length: usize,
        nal_length_size: usize,
    ) -> Result<Vec<NalUnit>, NalError> {
        let mut nal_units = Vec::new();
        let mut offset = 0;

        while offset + nal_length_size <= total_length {
            // Read NAL unit length (big-endian)
            let nal_length = match nal_length_size {
                4 => {
                    let bytes = std::slice::from_raw_parts(data_ptr.add(offset), 4);
                    u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize
                }
                2 => {
                    let bytes = std::slice::from_raw_parts(data_ptr.add(offset), 2);
                    u16::from_be_bytes([bytes[0], bytes[1]]) as usize
                }
                1 => *data_ptr.add(offset) as usize,
                _ => return Err(NalError::InvalidNalLength),
            };

            offset += nal_length_size;

            if offset + nal_length > total_length {
                return Err(NalError::BufferTooSmall);
            }

            // Read NAL unit data
            let nal_data = std::slice::from_raw_parts(data_ptr.add(offset), nal_length);
            let nal_type = nal_data[0] & 0x1F;

            nal_units.push(NalUnit {
                data: nal_data.to_vec(),
                nal_type,
            });

            offset += nal_length;
        }

        Ok(nal_units)
    }

    /// Extract timing information from a sample buffer.
    ///
    /// # Safety
    ///
    /// The sample buffer must be a valid sample buffer.
    pub unsafe fn get_timing(&self, sample_buffer: CMSampleBufferRef) -> SampleTiming {
        let pts = CMSampleBufferGetPresentationTimeStamp(sample_buffer);
        let dts = CMSampleBufferGetDecodeTimeStamp(sample_buffer);
        let duration = CMSampleBufferGetDuration(sample_buffer);

        // If DTS is invalid, use PTS
        let dts_value = if dts.flags & 1 != 0 {
            dts.value
        } else {
            pts.value
        };

        SampleTiming {
            pts: pts.value,
            dts: dts_value,
            duration: duration.value,
            timescale: pts.timescale,
        }
    }

    /// Check if a sample buffer represents a keyframe (sync sample).
    ///
    /// # Safety
    ///
    /// The sample buffer must be a valid sample buffer.
    pub unsafe fn is_keyframe(&self, sample_buffer: CMSampleBufferRef) -> bool {
        // Method 1: Check sample attachments for kCMSampleAttachmentKey_NotSync
        let attachments = CMSampleBufferGetSampleAttachmentsArray(sample_buffer, 0);
        if !attachments.is_null() {
            let first_attachment = CFArrayGetValueAtIndex(attachments as _, 0);
            if !first_attachment.is_null() {
                let not_sync =
                    CFDictionaryGetValue(first_attachment as _, kCMSampleAttachmentKey_NotSync);
                if not_sync.is_null() {
                    // kCMSampleAttachmentKey_NotSync is absent, so this is a sync sample
                    return true;
                }
                // If present and true, it's not a sync sample
                // CFBoolean: kCFBooleanTrue is non-null, kCFBooleanFalse is a different non-null
                // We need to check the actual value
                return is_cf_boolean_false(not_sync);
            }
        }

        // Method 2: Fallback - check if we have an IDR NAL unit
        if let Ok(nals) = self.extract_nal_units(sample_buffer) {
            return nals.iter().any(|nal| nal.is_idr());
        }

        false
    }

    /// Get the format description from a sample buffer.
    ///
    /// # Safety
    ///
    /// The sample buffer must be a valid sample buffer.
    pub unsafe fn get_format_description(
        &self,
        sample_buffer: CMSampleBufferRef,
    ) -> Option<CMFormatDescriptionRef> {
        let desc = CMSampleBufferGetFormatDescription(sample_buffer);
        if desc.is_null() {
            None
        } else {
            Some(desc)
        }
    }
}

/// Check if a CFTypeRef is CFBoolean false.
unsafe fn is_cf_boolean_false(value: CFTypeRef) -> bool {
    extern "C" {
        static kCFBooleanFalse: CFTypeRef;
    }
    value == kCFBooleanFalse
}

/// Convert a CMTime to a value in the given timescale.
pub fn convert_time(time: CMTime, target_timescale: i32) -> i64 {
    if time.timescale == target_timescale {
        return time.value;
    }
    // Scale the time value to the target timescale
    (time.value as f64 * target_timescale as f64 / time.timescale as f64).round() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_nal_unit_type_checks() {
        let idr = NalUnit {
            data: vec![0x65, 0x00],
            nal_type: 5,
        };
        assert!(idr.is_idr());
        assert!(idr.is_slice());
        assert!(!idr.is_sps());
        assert!(!idr.is_pps());

        let sps = NalUnit {
            data: vec![0x67, 0x00],
            nal_type: 7,
        };
        assert!(!sps.is_idr());
        assert!(!sps.is_slice());
        assert!(sps.is_sps());

        let pps = NalUnit {
            data: vec![0x68, 0x00],
            nal_type: 8,
        };
        assert!(pps.is_pps());
    }

    #[test]
    fn test_annex_b_conversion() {
        let nal = NalUnit {
            data: vec![0x67, 0x64, 0x00, 0x1f],
            nal_type: 7,
        };
        let annex_b = nal.to_annex_b();
        assert_eq!(&annex_b[..4], &[0x00, 0x00, 0x00, 0x01]);
        assert_eq!(&annex_b[4..], &[0x67, 0x64, 0x00, 0x1f]);
    }

    #[test]
    fn test_sample_timing_conversions() {
        let timing = SampleTiming {
            pts: 90000,
            dts: 90000,
            duration: 3000,
            timescale: 90000,
        };
        assert!((timing.pts_seconds() - 1.0).abs() < 0.0001);
        assert!((timing.duration_seconds() - 0.0333).abs() < 0.001);
    }
}
