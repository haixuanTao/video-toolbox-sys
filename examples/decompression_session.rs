//! Example: Creating a decompression session for H.264 decoding.
//!
//! This demonstrates how to set up a VideoToolbox decompression session.
//! Note: Actually decoding requires a CMFormatDescription from encoded data.
//!
//! Run with: cargo run --example decompression_session

extern crate core_foundation;
extern crate video_toolbox_sys;

use core_foundation::base::TCFType;
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::CFDictionary;
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;
use core_foundation_sys::base::OSStatus;
use core_foundation_sys::string::CFStringRef;
use core_media_sys::CMTime;
use core_video_sys::CVImageBufferRef;
use libc::c_void;
use video_toolbox_sys::decompression::{
    kVTDecodeInfo_Asynchronous, kVTDecodeInfo_FrameDropped,
    kVTVideoDecoderSpecification_EnableHardwareAcceleratedVideoDecoder, VTDecodeInfoFlags,
    VTDecompressionOutputCallbackRecord,
};

// Callback invoked when a decoded frame is ready
extern "C" fn decompression_output_callback(
    _decompression_output_ref_con: *mut c_void,
    _source_frame_ref_con: *mut c_void,
    status: OSStatus,
    info_flags: VTDecodeInfoFlags,
    image_buffer: CVImageBufferRef,
    presentation_time_stamp: CMTime,
    _presentation_duration: CMTime,
) {
    if status != 0 {
        println!("Decoding error: {}", status);
        return;
    }

    let async_decode = (info_flags & kVTDecodeInfo_Asynchronous) != 0;
    let dropped = (info_flags & kVTDecodeInfo_FrameDropped) != 0;

    if dropped {
        println!("Frame was dropped");
        return;
    }

    if image_buffer.is_null() {
        println!("No image buffer produced");
        return;
    }

    println!(
        "Decoded frame at PTS: {}/{} (async: {})",
        presentation_time_stamp.value, presentation_time_stamp.timescale, async_decode
    );

    // In a real application, you would:
    // 1. Lock the CVPixelBuffer base address
    // 2. Access the decoded pixel data
    // 3. Display or process the frame
    // 4. Unlock the buffer
}

fn main() {
    println!("Decompression Session Example\n");
    println!("This example shows how to configure a decompression session.");
    println!("Creating an actual session requires a CMVideoFormatDescription");
    println!("from encoded video data (e.g., H.264 SPS/PPS NAL units).\n");

    // Show how to create the decoder specification
    unsafe {
        let hw_key = CFString::wrap_under_get_rule(
            kVTVideoDecoderSpecification_EnableHardwareAcceleratedVideoDecoder as CFStringRef,
        );

        let decoder_spec = CFDictionary::from_CFType_pairs(&[(
            hw_key.as_CFType(),
            CFBoolean::true_value().as_CFType(),
        )]);

        println!("Decoder specification (hardware acceleration enabled):");
        println!("{:?}\n", decoder_spec);

        // Show pixel buffer attributes example
        // These tell VideoToolbox what format we want the decoded frames in
        let cv_pixel_format_type = CFString::new("PixelFormatType");
        let cv_width = CFString::new("Width");
        let cv_height = CFString::new("Height");

        // kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange = '420v' = 0x34323076
        let pixel_format = CFNumber::from(0x34323076i32);
        let width = CFNumber::from(1920i32);
        let height = CFNumber::from(1080i32);

        let dest_attrs = CFDictionary::from_CFType_pairs(&[
            (cv_pixel_format_type.as_CFType(), pixel_format.as_CFType()),
            (cv_width.as_CFType(), width.as_CFType()),
            (cv_height.as_CFType(), height.as_CFType()),
        ]);

        println!("Destination image buffer attributes:");
        println!("{:?}\n", dest_attrs);

        // Show callback structure
        let callback_record = VTDecompressionOutputCallbackRecord {
            decompressionOutputCallback: decompression_output_callback,
            decompressionOutputRefCon: std::ptr::null_mut(),
        };

        println!("Callback record created.");
        println!("\nTo create the session, you would call:");
        println!("  VTDecompressionSessionCreate(");
        println!("      allocator,");
        println!("      videoFormatDescription,  // From CMVideoFormatDescriptionCreateFromH264ParameterSets");
        println!("      decoderSpecification,");
        println!("      destinationImageBufferAttributes,");
        println!("      &callbackRecord,");
        println!("      &session");
        println!("  )");
        println!("\nThen decode frames with VTDecompressionSessionDecodeFrame()");

        // Suppress unused warning
        let _ = callback_record;
    }
}
