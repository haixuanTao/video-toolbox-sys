//! VideoToolbox error codes and utilities.

#![allow(non_upper_case_globals)]

use core_foundation_sys::base::OSStatus;

pub const kVTPropertyNotSupportedErr: OSStatus = -12900;
pub const kVTPropertyReadOnlyErr: OSStatus = -12901;
pub const kVTParameterErr: OSStatus = -12902;
pub const kVTInvalidSessionErr: OSStatus = -12903;
pub const kVTAllocationFailedErr: OSStatus = -12904;
pub const kVTPixelTransferNotSupportedErr: OSStatus = -12905; // c.f. -8961
pub const kVTCouldNotFindVideoDecoderErr: OSStatus = -12906;
pub const kVTCouldNotCreateInstanceErr: OSStatus = -12907;
pub const kVTCouldNotFindVideoEncoderErr: OSStatus = -12908;
pub const kVTVideoDecoderBadDataErr: OSStatus = -12909; // c.f. -8969
pub const kVTVideoDecoderUnsupportedDataFormatErr: OSStatus = -12910; // c.f. -8970
pub const kVTVideoDecoderMalfunctionErr: OSStatus = -12911; // c.f. -8960
pub const kVTVideoEncoderMalfunctionErr: OSStatus = -12912;
pub const kVTVideoDecoderNotAvailableNowErr: OSStatus = -12913;
pub const kVTImageRotationNotSupportedErr: OSStatus = -12914;
pub const kVTVideoEncoderNotAvailableNowErr: OSStatus = -12915;
pub const kVTFormatDescriptionChangeNotSupportedErr: OSStatus = -12916;
pub const kVTInsufficientSourceColorDataErr: OSStatus = -12917;
pub const kVTCouldNotCreateColorCorrectionDataErr: OSStatus = -12918;
pub const kVTColorSyncTransformConvertFailedErr: OSStatus = -12919;
pub const kVTVideoDecoderAuthorizationErr: OSStatus = -12210;
pub const kVTVideoEncoderAuthorizationErr: OSStatus = -12211;
pub const kVTColorCorrectionPixelTransferFailedErr: OSStatus = -12212;
pub const kVTMultiPassStorageIdentifierMismatchErr: OSStatus = -12213;
pub const kVTMultiPassStorageInvalidErr: OSStatus = -12214;
pub const kVTFrameSiloInvalidTimeStampErr: OSStatus = -12215;
pub const kVTFrameSiloInvalidTimeRangeErr: OSStatus = -12216;
pub const kVTCouldNotFindTemporalFilterErr: OSStatus = -12217;
pub const kVTPixelTransferNotPermittedErr: OSStatus = -12218;
pub const kVTColorCorrectionImageRotationFailedErr: OSStatus = -12219;
pub const kVTVideoDecoderRemovedErr: OSStatus = -17690;

/// Convert a VideoToolbox error status to a human-readable message.
///
/// # Example
///
/// ```
/// use video_toolbox_sys::errors::{vt_error_to_string, kVTInvalidSessionErr};
///
/// let msg = vt_error_to_string(kVTInvalidSessionErr);
/// assert_eq!(msg, "Invalid session");
/// ```
pub fn vt_error_to_string(status: OSStatus) -> &'static str {
    match status {
        0 => "Success",
        kVTPropertyNotSupportedErr => "Property not supported",
        kVTPropertyReadOnlyErr => "Property is read-only",
        kVTParameterErr => "Invalid parameter",
        kVTInvalidSessionErr => "Invalid session",
        kVTAllocationFailedErr => "Memory allocation failed",
        kVTPixelTransferNotSupportedErr => "Pixel transfer not supported",
        kVTCouldNotFindVideoDecoderErr => "Could not find video decoder",
        kVTCouldNotCreateInstanceErr => "Could not create instance",
        kVTCouldNotFindVideoEncoderErr => "Could not find video encoder",
        kVTVideoDecoderBadDataErr => "Video decoder received bad data",
        kVTVideoDecoderUnsupportedDataFormatErr => "Video decoder unsupported data format",
        kVTVideoDecoderMalfunctionErr => "Video decoder malfunction",
        kVTVideoEncoderMalfunctionErr => "Video encoder malfunction",
        kVTVideoDecoderNotAvailableNowErr => "Video decoder not available now",
        kVTImageRotationNotSupportedErr => "Image rotation not supported",
        kVTVideoEncoderNotAvailableNowErr => "Video encoder not available now",
        kVTFormatDescriptionChangeNotSupportedErr => "Format description change not supported",
        kVTInsufficientSourceColorDataErr => "Insufficient source color data",
        kVTCouldNotCreateColorCorrectionDataErr => "Could not create color correction data",
        kVTColorSyncTransformConvertFailedErr => "ColorSync transform convert failed",
        kVTVideoDecoderAuthorizationErr => "Video decoder authorization error",
        kVTVideoEncoderAuthorizationErr => "Video encoder authorization error",
        kVTColorCorrectionPixelTransferFailedErr => "Color correction pixel transfer failed",
        kVTMultiPassStorageIdentifierMismatchErr => "Multi-pass storage identifier mismatch",
        kVTMultiPassStorageInvalidErr => "Multi-pass storage invalid",
        kVTFrameSiloInvalidTimeStampErr => "Frame silo invalid timestamp",
        kVTFrameSiloInvalidTimeRangeErr => "Frame silo invalid time range",
        kVTCouldNotFindTemporalFilterErr => "Could not find temporal filter",
        kVTPixelTransferNotPermittedErr => "Pixel transfer not permitted",
        kVTColorCorrectionImageRotationFailedErr => "Color correction image rotation failed",
        kVTVideoDecoderRemovedErr => "Video decoder was removed",
        _ => "Unknown error",
    }
}

/// Check if an OSStatus indicates success.
#[inline]
pub fn is_success(status: OSStatus) -> bool {
    status == 0
}

/// Convert an OSStatus to a Result.
///
/// # Example
///
/// ```
/// use video_toolbox_sys::errors::status_to_result;
///
/// let result = status_to_result(0);
/// assert!(result.is_ok());
///
/// let result = status_to_result(-12903);
/// assert!(result.is_err());
/// ```
pub fn status_to_result(status: OSStatus) -> Result<(), OSStatus> {
    if status == 0 {
        Ok(())
    } else {
        Err(status)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_messages() {
        assert_eq!(vt_error_to_string(0), "Success");
        assert_eq!(vt_error_to_string(kVTInvalidSessionErr), "Invalid session");
        assert_eq!(vt_error_to_string(kVTCouldNotFindVideoEncoderErr), "Could not find video encoder");
        assert_eq!(vt_error_to_string(-99999), "Unknown error");
    }

    #[test]
    fn test_is_success() {
        assert!(is_success(0));
        assert!(!is_success(-12903));
    }

    #[test]
    fn test_status_to_result() {
        assert!(status_to_result(0).is_ok());
        assert_eq!(status_to_result(-12903), Err(-12903));
    }
}
