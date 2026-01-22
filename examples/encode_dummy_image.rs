//! Encode a dummy test pattern image to H.264 using hardware acceleration.
//!
//! This example creates a synthetic gradient image, wraps it in a CVPixelBuffer,
//! and encodes it using Apple's hardware H.264 encoder.
//!
//! Run with: cargo run --example encode_dummy_image --features helpers
//!
//! H.264 is used because it has simpler licensing terms compared to HEVC.

extern crate core_foundation;
extern crate video_toolbox_sys;

use core_foundation_sys::base::{CFTypeRef, OSStatus};
use core_media_sys::CMSampleBufferRef;
use libc::c_void;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use video_toolbox_sys::codecs;
use video_toolbox_sys::compression::{
    kVTEncodeInfo_FrameDropped, kVTProfileLevel_H264_High_AutoLevel,
    VTCompressionSessionCompleteFrames, VTCompressionSessionEncodeFrame,
    VTCompressionSessionInvalidate, VTCompressionSessionRef, VTEncodeInfoFlags,
};
use video_toolbox_sys::helpers::{
    create_pixel_buffer, run_while, CompressionSessionBuilder, PixelBufferConfig, PixelBufferGuard,
};

// Declare missing CoreMedia function
#[link(name = "CoreMedia", kind = "framework")]
extern "C" {
    fn CMSampleBufferGetTotalSampleSize(sbuf: CMSampleBufferRef) -> usize;
}

// Image dimensions
const WIDTH: usize = 1920;
const HEIGHT: usize = 1080;
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
    sample_buffer: *mut c_void,
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
    let data_size = unsafe { CMSampleBufferGetTotalSampleSize(sample_buffer as CMSampleBufferRef) };

    let frame_num = ENCODED_FRAMES.fetch_add(1, Ordering::SeqCst) + 1;
    TOTAL_BYTES.fetch_add(data_size, Ordering::SeqCst);

    println!("  Encoded frame {}: {} bytes", frame_num, data_size);

    if frame_num >= NUM_FRAMES {
        ENCODING_DONE.store(true, Ordering::SeqCst);
    }
}

/// Create a CVPixelBuffer with a gradient test pattern using library helpers
fn create_test_image(frame_number: usize) -> video_toolbox_sys::cv_types::CVPixelBufferRef {
    // Create pixel buffer using helper
    let config = PixelBufferConfig::new(WIDTH, HEIGHT);
    let pixel_buffer = create_pixel_buffer(&config).expect("Failed to create CVPixelBuffer");

    // Lock buffer and fill with gradient pattern
    unsafe {
        let guard = PixelBufferGuard::lock(pixel_buffer).expect("Failed to lock buffer");
        let base_address = guard.base_address();
        let bytes_per_row = guard.bytes_per_row();

        // Create a moving gradient pattern
        let offset = (frame_number * 10) % 256;

        for y in 0..HEIGHT {
            let row = base_address.add(y * bytes_per_row);
            for x in 0..WIDTH {
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
        // Guard automatically unlocks when dropped
    }

    pixel_buffer
}

fn main() {
    println!("H.264 Hardware Encoding Example");
    println!("===============================");
    println!("Resolution: {}x{}", WIDTH, HEIGHT);
    println!("Frames to encode: {}\n", NUM_FRAMES);

    println!("Creating H.264 compression session...");

    // Create compression session using builder
    let session: VTCompressionSessionRef = unsafe {
        CompressionSessionBuilder::new(WIDTH as i32, HEIGHT as i32, codecs::video::H264)
            .hardware_accelerated(true)
            .bitrate(8_000_000)
            .frame_rate(30.0)
            .keyframe_interval(30)
            .real_time(true)
            .profile_level(kVTProfileLevel_H264_High_AutoLevel)
            .build_with_context(Some(compression_output_callback), ptr::null_mut())
            .expect("Failed to create compression session")
    };

    println!("Compression session created successfully!");
    println!("Session configured:");
    println!("  Profile: H.264 High (Auto Level)");
    println!("  Bitrate: 8 Mbps");
    println!("  Frame rate: 30 fps");
    println!("  Keyframe interval: 30 frames\n");

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

        let encode_status = unsafe {
            VTCompressionSessionEncodeFrame(
                session,
                pixel_buffer,
                pts,
                duration,
                ptr::null(),     // frame properties
                ptr::null_mut(), // source frame refcon
                &mut info_flags,
            )
        };

        if encode_status != 0 {
            println!("Failed to encode frame {}: {}", frame_num, encode_status);
        }

        // Release the pixel buffer
        unsafe {
            core_foundation_sys::base::CFRelease(pixel_buffer as CFTypeRef);
        }
    }

    // Signal that we're done submitting frames
    let complete_time = core_media_sys::CMTime {
        value: NUM_FRAMES as i64,
        timescale: 30,
        flags: 1,
        epoch: 0,
    };
    unsafe {
        VTCompressionSessionCompleteFrames(session, complete_time);
    }

    // Wait for encoding to complete using run_while helper
    run_while(
        || !ENCODING_DONE.load(Ordering::SeqCst),
        std::time::Duration::from_millis(10),
        Some(std::time::Duration::from_secs(10)),
    );

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
    unsafe {
        VTCompressionSessionInvalidate(session);
    }
    println!("\nSession invalidated.");
}
