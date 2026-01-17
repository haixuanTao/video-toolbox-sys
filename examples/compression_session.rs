//! Example: Creating a compression session for H.264 encoding.
//!
//! This demonstrates how to set up a VideoToolbox compression session.
//! Note: Actually encoding frames requires CVPixelBuffer data from CoreVideo.
//!
//! Run with: cargo run --example compression_session

extern crate core_foundation;
extern crate video_toolbox_sys;

use core_foundation_sys::base::{kCFAllocatorDefault, CFTypeRef, OSStatus};
use core_foundation_sys::dictionary::CFDictionaryRef;
use core_foundation_sys::string::CFStringRef;
use core_media_sys::CMSampleBufferRef;
use libc::c_void;
use std::ptr;
use video_toolbox_sys::compression::{
    kVTCompressionPropertyKey_AverageBitRate, kVTCompressionPropertyKey_MaxKeyFrameInterval,
    kVTCompressionPropertyKey_ProfileLevel, kVTCompressionPropertyKey_RealTime,
    kVTProfileLevel_H264_High_AutoLevel,
    kVTVideoEncoderSpecification_EnableHardwareAcceleratedVideoEncoder, VTCompressionOutputCallback,
    VTCompressionSessionCreate, VTCompressionSessionInvalidate,
    VTCompressionSessionPrepareToEncodeFrames, VTCompressionSessionRef, VTEncodeInfoFlags,
};
use video_toolbox_sys::session::VTSessionSetProperty;

// H.264 codec FourCC
const K_CM_VIDEO_CODEC_TYPE_H264: u32 = 0x61766331; // 'avc1'

// Callback invoked when an encoded frame is ready
extern "C" fn compression_output_callback(
    _output_callback_ref_con: *mut c_void,
    _source_frame_ref_con: *mut c_void,
    status: OSStatus,
    info_flags: VTEncodeInfoFlags,
    sample_buffer: CMSampleBufferRef,
) {
    if status != 0 {
        println!("Encoding error: {}", status);
        return;
    }

    if sample_buffer.is_null() {
        println!("No sample buffer produced");
        return;
    }

    let dropped = (info_flags & 0x2) != 0; // kVTEncodeInfo_FrameDropped
    if dropped {
        println!("Frame was dropped");
    } else {
        println!("Encoded frame received!");
        // In a real application, you would:
        // 1. Get the CMBlockBuffer from the sample buffer
        // 2. Extract the H.264 NAL units
        // 3. Write to file or send over network
    }
}

fn main() {
    use core_foundation::base::TCFType;
    use core_foundation::boolean::CFBoolean;
    use core_foundation::dictionary::CFDictionary;
    use core_foundation::number::CFNumber;
    use core_foundation::string::CFString;

    let width: i32 = 1920;
    let height: i32 = 1080;

    println!("Creating H.264 compression session ({}x{})", width, height);

    unsafe {
        // Create encoder specification requesting hardware acceleration
        let hw_key = CFString::wrap_under_get_rule(
            kVTVideoEncoderSpecification_EnableHardwareAcceleratedVideoEncoder as CFStringRef,
        );
        let encoder_spec = CFDictionary::from_CFType_pairs(&[(
            hw_key.as_CFType(),
            CFBoolean::true_value().as_CFType(),
        )]);

        let mut session: VTCompressionSessionRef = ptr::null_mut();

        let status = VTCompressionSessionCreate(
            kCFAllocatorDefault,
            width,
            height,
            K_CM_VIDEO_CODEC_TYPE_H264,
            encoder_spec.as_concrete_TypeRef() as CFDictionaryRef,
            ptr::null(),         // sourceImageBufferAttributes (let VT choose)
            kCFAllocatorDefault, // compressedDataAllocator
            compression_output_callback as VTCompressionOutputCallback,
            ptr::null_mut(),     // outputCallbackRefCon
            &mut session,
        );

        if status != 0 {
            println!("Failed to create compression session: {}", status);
            return;
        }

        println!("Compression session created successfully!");

        // Configure session properties
        // Set profile to H.264 High (auto level)
        let profile_key =
            CFString::wrap_under_get_rule(kVTCompressionPropertyKey_ProfileLevel as CFStringRef);
        let profile_value =
            CFString::wrap_under_get_rule(kVTProfileLevel_H264_High_AutoLevel as CFStringRef);
        VTSessionSetProperty(
            session,
            profile_key.as_concrete_TypeRef() as CFStringRef,
            profile_value.as_concrete_TypeRef() as CFTypeRef,
        );
        println!("  Profile: H.264 High (Auto Level)");

        // Set bitrate to 5 Mbps
        let bitrate_key =
            CFString::wrap_under_get_rule(kVTCompressionPropertyKey_AverageBitRate as CFStringRef);
        let bitrate_value = CFNumber::from(5_000_000i64);
        VTSessionSetProperty(
            session,
            bitrate_key.as_concrete_TypeRef() as CFStringRef,
            bitrate_value.as_concrete_TypeRef() as CFTypeRef,
        );
        println!("  Bitrate: 5 Mbps");

        // Set keyframe interval to 60 frames
        let keyframe_key = CFString::wrap_under_get_rule(
            kVTCompressionPropertyKey_MaxKeyFrameInterval as CFStringRef,
        );
        let keyframe_value = CFNumber::from(60i32);
        VTSessionSetProperty(
            session,
            keyframe_key.as_concrete_TypeRef() as CFStringRef,
            keyframe_value.as_concrete_TypeRef() as CFTypeRef,
        );
        println!("  Keyframe interval: 60 frames");

        // Enable real-time encoding
        let realtime_key =
            CFString::wrap_under_get_rule(kVTCompressionPropertyKey_RealTime as CFStringRef);
        VTSessionSetProperty(
            session,
            realtime_key.as_concrete_TypeRef() as CFStringRef,
            CFBoolean::true_value().as_concrete_TypeRef() as CFTypeRef,
        );
        println!("  Real-time: enabled");

        // Prepare the session for encoding
        let prep_status = VTCompressionSessionPrepareToEncodeFrames(session);
        if prep_status != 0 {
            println!("Failed to prepare for encoding: {}", prep_status);
        } else {
            println!("\nSession prepared and ready for encoding!");
            println!("\nTo encode frames, you would:");
            println!("  1. Create CVPixelBuffer with your image data");
            println!("  2. Call VTCompressionSessionEncodeFrame()");
            println!("  3. Handle encoded data in the callback");
            println!("  4. Call VTCompressionSessionCompleteFrames() when done");
        }

        // Clean up
        VTCompressionSessionInvalidate(session);
        println!("\nSession invalidated.");
    }
}
