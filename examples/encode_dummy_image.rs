//! Encode a dummy test pattern image to HEVC using hardware acceleration.
//!
//! This example creates a synthetic gradient image, wraps it in a CVPixelBuffer,
//! and encodes it using Apple's hardware HEVC encoder.
//!
//! Run with: cargo run --example encode_dummy_image
//!
//! Note: AV1 encoding is not yet available on macOS (only decode).
//! This example uses HEVC which has hardware encoder support.

extern crate core_foundation;
extern crate video_toolbox_sys;

use core_foundation::base::TCFType;
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::CFDictionary;
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;
use core_foundation_sys::base::{kCFAllocatorDefault, CFTypeRef, OSStatus};
use core_foundation_sys::dictionary::CFDictionaryRef;
use core_foundation_sys::string::CFStringRef;
use core_media_sys::CMSampleBufferRef;

// Declare missing CoreMedia function
#[link(name = "CoreMedia", kind = "framework")]
extern "C" {
    fn CMSampleBufferGetTotalSampleSize(sbuf: CMSampleBufferRef) -> usize;
}
use core_video_sys::{
    kCVPixelBufferCGBitmapContextCompatibilityKey, kCVPixelBufferCGImageCompatibilityKey,
    kCVPixelBufferHeightKey, kCVPixelBufferPixelFormatTypeKey, kCVPixelBufferWidthKey,
    kCVReturnSuccess, CVPixelBufferCreate, CVPixelBufferGetBaseAddress,
    CVPixelBufferGetBytesPerRow, CVPixelBufferLockBaseAddress, CVPixelBufferRef,
    CVPixelBufferUnlockBaseAddress,
};
use libc::c_void;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use video_toolbox_sys::compression::{
    kVTCompressionPropertyKey_AverageBitRate, kVTCompressionPropertyKey_ExpectedFrameRate,
    kVTCompressionPropertyKey_MaxKeyFrameInterval, kVTCompressionPropertyKey_ProfileLevel,
    kVTCompressionPropertyKey_RealTime, kVTEncodeInfo_FrameDropped,
    kVTProfileLevel_HEVC_Main_AutoLevel,
    kVTVideoEncoderSpecification_EnableHardwareAcceleratedVideoEncoder,
    VTCompressionOutputCallback, VTCompressionSessionCompleteFrames, VTCompressionSessionCreate,
    VTCompressionSessionEncodeFrame, VTCompressionSessionInvalidate,
    VTCompressionSessionPrepareToEncodeFrames, VTCompressionSessionRef, VTEncodeInfoFlags,
};
use video_toolbox_sys::session::VTSessionSetProperty;

// HEVC codec FourCC: 'hvc1'
const K_CM_VIDEO_CODEC_TYPE_HEVC: u32 = 0x68766331;

// kCVPixelFormatType_32BGRA
const K_CV_PIXEL_FORMAT_TYPE_32BGRA: u32 = 0x42475241; // 'BGRA'

// Image dimensions
const WIDTH: i32 = 1920;
const HEIGHT: i32 = 1080;
const NUM_FRAMES: usize = 30;

// Global counters for the callback
static ENCODED_FRAMES: AtomicUsize = AtomicUsize::new(0);
static TOTAL_BYTES: AtomicUsize = AtomicUsize::new(0);
static ENCODING_DONE: AtomicBool = AtomicBool::new(false);

// Callback invoked when an encoded frame is ready
extern "C" fn compression_output_callback(
    _output_callback_ref_con: *mut c_void,
    _source_frame_ref_con: *mut c_void,
    status: OSStatus,
    info_flags: VTEncodeInfoFlags,
    sample_buffer: CMSampleBufferRef,
) {
    if status != 0 {
        println!("  Encoding error: OSStatus {}", status);
        return;
    }

    if sample_buffer.is_null() {
        return;
    }

    let dropped = (info_flags & kVTEncodeInfo_FrameDropped) != 0;
    if dropped {
        println!("  Frame dropped");
        return;
    }

    // Get the size of the encoded data
    let data_size = unsafe { CMSampleBufferGetTotalSampleSize(sample_buffer) };

    let frame_num = ENCODED_FRAMES.fetch_add(1, Ordering::SeqCst) + 1;
    TOTAL_BYTES.fetch_add(data_size, Ordering::SeqCst);

    println!("  Encoded frame {}: {} bytes", frame_num, data_size);

    if frame_num >= NUM_FRAMES {
        ENCODING_DONE.store(true, Ordering::SeqCst);
    }
}

/// Create a CVPixelBuffer with a gradient test pattern
fn create_test_image(frame_number: usize) -> CVPixelBufferRef {
    unsafe {
        let mut pixel_buffer: CVPixelBufferRef = ptr::null_mut();

        // Create pixel buffer attributes
        let format_key = CFString::wrap_under_get_rule(kCVPixelBufferPixelFormatTypeKey);
        let width_key = CFString::wrap_under_get_rule(kCVPixelBufferWidthKey);
        let height_key = CFString::wrap_under_get_rule(kCVPixelBufferHeightKey);
        let cg_compat_key = CFString::wrap_under_get_rule(kCVPixelBufferCGImageCompatibilityKey);
        let cg_bitmap_key =
            CFString::wrap_under_get_rule(kCVPixelBufferCGBitmapContextCompatibilityKey);

        let attrs = CFDictionary::from_CFType_pairs(&[
            (
                format_key.as_CFType(),
                CFNumber::from(K_CV_PIXEL_FORMAT_TYPE_32BGRA as i32).as_CFType(),
            ),
            (width_key.as_CFType(), CFNumber::from(WIDTH).as_CFType()),
            (height_key.as_CFType(), CFNumber::from(HEIGHT).as_CFType()),
            (
                cg_compat_key.as_CFType(),
                CFBoolean::true_value().as_CFType(),
            ),
            (
                cg_bitmap_key.as_CFType(),
                CFBoolean::true_value().as_CFType(),
            ),
        ]);

        let status = CVPixelBufferCreate(
            kCFAllocatorDefault,
            WIDTH as usize,
            HEIGHT as usize,
            K_CV_PIXEL_FORMAT_TYPE_32BGRA,
            attrs.as_concrete_TypeRef() as CFDictionaryRef,
            &mut pixel_buffer,
        );

        if status != kCVReturnSuccess {
            panic!("Failed to create CVPixelBuffer: {}", status);
        }

        // Lock the buffer to write pixels
        CVPixelBufferLockBaseAddress(pixel_buffer, 0);

        let base_address = CVPixelBufferGetBaseAddress(pixel_buffer) as *mut u8;
        let bytes_per_row = CVPixelBufferGetBytesPerRow(pixel_buffer);

        // Create a moving gradient pattern
        let offset = (frame_number * 10) % 256;

        for y in 0..HEIGHT as usize {
            let row = base_address.add(y * bytes_per_row);
            for x in 0..WIDTH as usize {
                let pixel = row.add(x * 4);
                // BGRA format
                let r = ((x + offset) % 256) as u8;
                let g = ((y + offset) % 256) as u8;
                let b = (((x + y) / 2 + offset) % 256) as u8;
                *pixel.add(0) = b; // B
                *pixel.add(1) = g; // G
                *pixel.add(2) = r; // R
                *pixel.add(3) = 255; // A
            }
        }

        CVPixelBufferUnlockBaseAddress(pixel_buffer, 0);

        pixel_buffer
    }
}

fn main() {
    println!("HEVC Hardware Encoding Example");
    println!("===============================");
    println!("Resolution: {}x{}", WIDTH, HEIGHT);
    println!("Frames to encode: {}\n", NUM_FRAMES);

    unsafe {
        // Create encoder specification requesting hardware acceleration
        let hw_key = CFString::wrap_under_get_rule(
            kVTVideoEncoderSpecification_EnableHardwareAcceleratedVideoEncoder as CFStringRef,
        );
        let encoder_spec = CFDictionary::from_CFType_pairs(&[(
            hw_key.as_CFType(),
            CFBoolean::true_value().as_CFType(),
        )]);

        // Create source image buffer attributes
        let format_key = CFString::wrap_under_get_rule(kCVPixelBufferPixelFormatTypeKey);
        let width_key = CFString::wrap_under_get_rule(kCVPixelBufferWidthKey);
        let height_key = CFString::wrap_under_get_rule(kCVPixelBufferHeightKey);

        let source_attrs = CFDictionary::from_CFType_pairs(&[
            (
                format_key.as_CFType(),
                CFNumber::from(K_CV_PIXEL_FORMAT_TYPE_32BGRA as i32).as_CFType(),
            ),
            (width_key.as_CFType(), CFNumber::from(WIDTH).as_CFType()),
            (height_key.as_CFType(), CFNumber::from(HEIGHT).as_CFType()),
        ]);

        let mut session: VTCompressionSessionRef = ptr::null_mut();

        println!("Creating HEVC compression session...");

        let status = VTCompressionSessionCreate(
            kCFAllocatorDefault,
            WIDTH,
            HEIGHT,
            K_CM_VIDEO_CODEC_TYPE_HEVC,
            encoder_spec.as_concrete_TypeRef() as CFDictionaryRef,
            source_attrs.as_concrete_TypeRef() as CFDictionaryRef,
            kCFAllocatorDefault,
            compression_output_callback as VTCompressionOutputCallback,
            ptr::null_mut(),
            &mut session,
        );

        if status != 0 {
            println!("Failed to create compression session: OSStatus {}", status);
            println!("\nPossible reasons:");
            println!("  - HEVC encoder not available on this system");
            println!("  - Try using H.264 (codec type 0x61766331) instead");
            return;
        }

        println!("Compression session created successfully!");

        // Configure session properties
        // Profile: HEVC Main (auto level)
        let profile_key =
            CFString::wrap_under_get_rule(kVTCompressionPropertyKey_ProfileLevel as CFStringRef);
        let profile_value =
            CFString::wrap_under_get_rule(kVTProfileLevel_HEVC_Main_AutoLevel as CFStringRef);
        VTSessionSetProperty(
            session,
            profile_key.as_concrete_TypeRef() as CFStringRef,
            profile_value.as_concrete_TypeRef() as CFTypeRef,
        );

        // Bitrate: 8 Mbps
        let bitrate_key =
            CFString::wrap_under_get_rule(kVTCompressionPropertyKey_AverageBitRate as CFStringRef);
        let bitrate_value = CFNumber::from(8_000_000i64);
        VTSessionSetProperty(
            session,
            bitrate_key.as_concrete_TypeRef() as CFStringRef,
            bitrate_value.as_concrete_TypeRef() as CFTypeRef,
        );

        // Expected frame rate: 30 fps
        let fps_key = CFString::wrap_under_get_rule(
            kVTCompressionPropertyKey_ExpectedFrameRate as CFStringRef,
        );
        let fps_value = CFNumber::from(30.0f64);
        VTSessionSetProperty(
            session,
            fps_key.as_concrete_TypeRef() as CFStringRef,
            fps_value.as_concrete_TypeRef() as CFTypeRef,
        );

        // Keyframe interval: every 30 frames (1 second at 30fps)
        let keyframe_key = CFString::wrap_under_get_rule(
            kVTCompressionPropertyKey_MaxKeyFrameInterval as CFStringRef,
        );
        let keyframe_value = CFNumber::from(30i32);
        VTSessionSetProperty(
            session,
            keyframe_key.as_concrete_TypeRef() as CFStringRef,
            keyframe_value.as_concrete_TypeRef() as CFTypeRef,
        );

        // Real-time encoding
        let realtime_key =
            CFString::wrap_under_get_rule(kVTCompressionPropertyKey_RealTime as CFStringRef);
        VTSessionSetProperty(
            session,
            realtime_key.as_concrete_TypeRef() as CFStringRef,
            CFBoolean::true_value().as_concrete_TypeRef() as CFTypeRef,
        );

        println!("Session configured:");
        println!("  Profile: HEVC Main (Auto Level)");
        println!("  Bitrate: 8 Mbps");
        println!("  Frame rate: 30 fps");
        println!("  Keyframe interval: 30 frames\n");

        // Prepare for encoding
        let prep_status = VTCompressionSessionPrepareToEncodeFrames(session);
        if prep_status != 0 {
            println!("Failed to prepare session: {}", prep_status);
            VTCompressionSessionInvalidate(session);
            return;
        }

        println!("Encoding {} frames...\n", NUM_FRAMES);

        let start_time = std::time::Instant::now();

        // Encode frames
        for frame_num in 0..NUM_FRAMES {
            // Create a test image with a moving pattern
            let pixel_buffer = create_test_image(frame_num);

            // Create presentation timestamp (30 fps = 1/30 second per frame)
            let pts = core_media_sys::CMTime {
                value: frame_num as i64,
                timescale: 30,
                flags: 1, // kCMTimeFlags_Valid
                epoch: 0,
            };

            // Duration of one frame at 30fps
            let duration = core_media_sys::CMTime {
                value: 1,
                timescale: 30,
                flags: 1,
                epoch: 0,
            };

            let mut info_flags: VTEncodeInfoFlags = 0;

            let encode_status = VTCompressionSessionEncodeFrame(
                session,
                pixel_buffer,
                pts,
                duration,
                ptr::null(),     // frame properties
                ptr::null_mut(), // source frame refcon
                &mut info_flags,
            );

            if encode_status != 0 {
                println!("Failed to encode frame {}: {}", frame_num, encode_status);
            }

            // Release the pixel buffer
            core_foundation_sys::base::CFRelease(pixel_buffer as CFTypeRef);
        }

        // Signal that we're done submitting frames
        let complete_time = core_media_sys::CMTime {
            value: NUM_FRAMES as i64,
            timescale: 30,
            flags: 1,
            epoch: 0,
        };
        VTCompressionSessionCompleteFrames(session, complete_time);

        // Wait for encoding to complete
        while !ENCODING_DONE.load(Ordering::SeqCst) {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        let elapsed = start_time.elapsed();

        // Print summary
        let total_frames = ENCODED_FRAMES.load(Ordering::SeqCst);
        let total_bytes = TOTAL_BYTES.load(Ordering::SeqCst);

        println!("\n===============================");
        println!("Encoding complete!");
        println!("  Frames encoded: {}", total_frames);
        println!(
            "  Total size: {} bytes ({:.2} KB)",
            total_bytes,
            total_bytes as f64 / 1024.0
        );
        println!(
            "  Average frame size: {:.0} bytes",
            total_bytes as f64 / total_frames as f64
        );
        println!("  Time elapsed: {:.2?}", elapsed);
        println!(
            "  Encoding speed: {:.1} fps",
            total_frames as f64 / elapsed.as_secs_f64()
        );

        // Clean up
        VTCompressionSessionInvalidate(session);
        println!("\nSession invalidated.");
    }
}
