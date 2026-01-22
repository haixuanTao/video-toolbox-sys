//! Stream webcam video as CMAF (Common Media Application Format).
//!
//! This example demonstrates capturing video from a webcam, encoding it with
//! H.264 using VideoToolbox hardware acceleration, and outputting CMAF
//! segments suitable for:
//! - Live streaming (DASH/HLS)
//! - Media Source Extensions (MSE) in browsers
//! - Low-latency video delivery
//!
//! # Output Files
//!
//! - `init.mp4` - Initialization segment (ftyp + moov with SPS/PPS)
//! - `segment_001.m4s`, `segment_002.m4s`, ... - Media segments
//!
//! The segments can be:
//! - Played sequentially in VLC: `cat init.mp4 segment_*.m4s > full.mp4`
//! - Used with DASH/HLS manifests
//! - Fed to Media Source Extensions in browsers
//!
//! # Usage
//!
//! ```bash
//! cargo run --example webcam_cmaf_stream
//! ```
//!
//! # Note
//!
//! Camera permissions may be required on macOS. Grant access when prompted.

use core_foundation_sys::base::OSStatus;
use core_media_sys::{CMSampleBufferRef, CMTime};
use libc::c_void;
use objc2::rc::Retained;
use objc2::runtime::{Bool, Sel};
use objc2::{class, msg_send};
use objc2_av_foundation::{
    AVCaptureDevice, AVCaptureDeviceInput, AVCaptureSession, AVCaptureVideoDataOutput,
    AVMediaTypeVideo,
};
use objc2_foundation::{ns_string, NSNumber, NSObject};
use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;
use video_toolbox_sys::codecs;
use video_toolbox_sys::compression::{
    kVTEncodeInfo_FrameDropped, kVTProfileLevel_H264_High_AutoLevel,
    VTCompressionSessionCompleteFrames, VTCompressionSessionEncodeFrame,
    VTCompressionSessionInvalidate, VTCompressionSessionRef, VTEncodeInfoFlags,
};
use video_toolbox_sys::cv_types::CVPixelBufferRef;
use video_toolbox_sys::helpers::{
    create_capture_delegate, create_dispatch_queue, run_for_duration, set_sample_buffer_delegate,
    CompressionSessionBuilder, DelegateCallback, CmafConfig, CmafMuxer, NalExtractor,
};

// Recording parameters
const WIDTH: i32 = 1280;
const HEIGHT: i32 = 720;
const FRAME_RATE: f64 = 30.0;
const BITRATE: i64 = 4_000_000; // 4 Mbps
const RECORD_DURATION_SECS: u64 = 10;
const FRAGMENT_DURATION_MS: u32 = 2000; // 2-second fragments

// Global state
static FRAME_COUNT: AtomicUsize = AtomicUsize::new(0);
static ENCODED_FRAMES: AtomicUsize = AtomicUsize::new(0);
static SEGMENT_COUNT: AtomicUsize = AtomicUsize::new(0);
static SHOULD_STOP: AtomicBool = AtomicBool::new(false);

// Thread-safe wrapper for muxer state
struct MuxerContext {
    muxer: CmafMuxer,
    extractor: NalExtractor,
    output_dir: PathBuf,
    initialized: bool,
}

unsafe impl Send for MuxerContext {}
unsafe impl Sync for MuxerContext {}

static MUXER_CONTEXT: Mutex<Option<MuxerContext>> = Mutex::new(None);

// Global compression session
static mut COMPRESSION_SESSION: VTCompressionSessionRef = ptr::null_mut();

// CoreMedia FFI
#[link(name = "CoreMedia", kind = "framework")]
extern "C" {
    fn CMSampleBufferGetImageBuffer(sbuf: *const c_void) -> CVPixelBufferRef;
}

/// Compression output callback - called when VideoToolbox has encoded a frame.
extern "C" fn compression_output_callback(
    _output_callback_ref_con: *mut c_void,
    _source_frame_ref_con: *mut c_void,
    status: OSStatus,
    info_flags: VTEncodeInfoFlags,
    sample_buffer: CMSampleBufferRef,
) {
    if status != 0 {
        eprintln!("Encoding error: OSStatus {}", status);
        return;
    }

    if sample_buffer.is_null() {
        return;
    }

    let dropped = (info_flags & kVTEncodeInfo_FrameDropped) != 0;
    if dropped {
        eprintln!("Frame dropped");
        return;
    }

    let mut ctx_guard = MUXER_CONTEXT.lock().unwrap();
    let ctx = match ctx_guard.as_mut() {
        Some(c) => c,
        None => return,
    };

    unsafe {
        // Get format description and initialize muxer if needed
        if !ctx.initialized {
            if let Some(format_desc) = ctx.extractor.get_format_description(sample_buffer) {
                match ctx.extractor.extract_parameter_sets(format_desc) {
                    Ok(params) => {
                        match ctx.extractor.get_dimensions(format_desc) {
                            Ok(dims) => {
                                // Create initialization segment
                                let init_segment = ctx.muxer.create_init_segment(
                                    &params.sps,
                                    &params.pps,
                                    dims.width,
                                    dims.height,
                                );

                                // Write init segment to file
                                let init_path = ctx.output_dir.join("init.mp4");
                                if let Ok(mut file) = File::create(&init_path) {
                                    if file.write_all(&init_segment).is_ok() {
                                        println!(
                                            "  Created initialization segment: {} ({} bytes)",
                                            init_path.display(),
                                            init_segment.len()
                                        );
                                        ctx.initialized = true;
                                    }
                                }
                            }
                            Err(e) => eprintln!("Failed to get dimensions: {}", e),
                        }
                    }
                    Err(e) => eprintln!("Failed to extract parameter sets: {}", e),
                }
            }
        }

        if !ctx.initialized {
            return;
        }

        // Extract NAL units from the encoded frame
        let nal_units = match ctx.extractor.extract_nal_units(sample_buffer) {
            Ok(nals) => nals,
            Err(e) => {
                eprintln!("Failed to extract NAL units: {}", e);
                return;
            }
        };

        // Get timing information
        let timing = ctx.extractor.get_timing(sample_buffer);
        let is_keyframe = ctx.extractor.is_keyframe(sample_buffer);

        // Convert timing to muxer's timescale (90000)
        let target_timescale = 90000i32;
        let pts = (timing.pts as f64 * target_timescale as f64 / timing.timescale as f64) as i64;
        let dts = (timing.dts as f64 * target_timescale as f64 / timing.timescale as f64) as i64;
        let duration =
            (timing.duration as f64 * target_timescale as f64 / timing.timescale as f64) as u32;

        // Add frame to muxer
        if let Some(segment) = ctx.muxer.add_frame(&nal_units, pts, dts, duration, is_keyframe) {
            // Write segment to file
            let segment_num = SEGMENT_COUNT.fetch_add(1, Ordering::SeqCst) + 1;
            let segment_path = ctx.output_dir.join(format!("segment_{:03}.m4s", segment_num));

            if let Ok(mut file) = File::create(&segment_path) {
                if file.write_all(&segment).is_ok() {
                    println!(
                        "  Created segment {}: {} ({} bytes)",
                        segment_num,
                        segment_path.display(),
                        segment.len()
                    );
                }
            }
        }

        let frame_num = ENCODED_FRAMES.fetch_add(1, Ordering::SeqCst) + 1;
        if frame_num % 30 == 0 {
            println!("  Encoded {} frames...", frame_num);
        }
    }
}

fn create_compression_session() -> Result<VTCompressionSessionRef, OSStatus> {
    unsafe {
        CompressionSessionBuilder::new(WIDTH, HEIGHT, codecs::video::H264)
            .pixel_format(codecs::pixel::BGRA32)
            .hardware_accelerated(true)
            .bitrate(BITRATE)
            .frame_rate(FRAME_RATE)
            .keyframe_interval(FRAME_RATE as i32 * 2) // Keyframe every 2 seconds
            .real_time(true)
            .profile_level(kVTProfileLevel_H264_High_AutoLevel)
            .build_with_context(Some(compression_output_callback), ptr::null_mut())
    }
}

// Delegate callback for video frame capture
extern "C" fn capture_output_did_output(
    _this: *mut c_void,
    _cmd: Sel,
    _output: *mut c_void,
    sample_buffer: *mut c_void,
    _connection: *mut c_void,
) {
    unsafe {
        if SHOULD_STOP.load(Ordering::SeqCst) {
            return;
        }

        // Get pixel buffer from sample buffer
        let pixel_buffer = CMSampleBufferGetImageBuffer(sample_buffer);
        if pixel_buffer.is_null() {
            eprintln!("Warning: Got null pixel buffer from sample buffer");
            return;
        }

        let frame_num = FRAME_COUNT.fetch_add(1, Ordering::SeqCst);
        if frame_num == 0 {
            println!("  First frame received!");
        }

        // Create presentation timestamp
        let pts = CMTime {
            value: frame_num as i64,
            timescale: FRAME_RATE as i32,
            flags: 1,
            epoch: 0,
        };

        let duration = CMTime {
            value: 1,
            timescale: FRAME_RATE as i32,
            flags: 1,
            epoch: 0,
        };

        // Encode the frame
        let mut info_flags: VTEncodeInfoFlags = 0;

        let status = VTCompressionSessionEncodeFrame(
            COMPRESSION_SESSION,
            pixel_buffer,
            pts,
            duration,
            ptr::null(),
            ptr::null_mut(),
            &mut info_flags,
        );

        if status != 0 {
            eprintln!("Failed to encode frame: OSStatus {}", status);
        }
    }
}

fn main() {
    println!("Webcam to fMP4 Streaming Example");
    println!("=================================");
    println!("Resolution: {}x{}", WIDTH, HEIGHT);
    println!("Frame rate: {} fps", FRAME_RATE);
    println!("Bitrate: {} Mbps", BITRATE / 1_000_000);
    println!("Fragment duration: {} ms", FRAGMENT_DURATION_MS);
    println!("Duration: {} seconds\n", RECORD_DURATION_SECS);

    // Create output directory
    let output_dir = std::env::current_dir()
        .unwrap()
        .join("cmaf_output");

    if let Err(e) = fs::create_dir_all(&output_dir) {
        eprintln!("Failed to create output directory: {}", e);
        return;
    }

    // Clean up any existing segments
    if let Ok(entries) = fs::read_dir(&output_dir) {
        for entry in entries.flatten() {
            let _ = fs::remove_file(entry.path());
        }
    }

    println!("Output directory: {}\n", output_dir.display());

    unsafe {
        // Initialize muxer context
        {
            let muxer = CmafMuxer::new(CmafConfig {
                fragment_duration_ms: FRAGMENT_DURATION_MS,
                timescale: 90000,
            });

            let mut ctx = MUXER_CONTEXT.lock().unwrap();
            *ctx = Some(MuxerContext {
                muxer,
                extractor: NalExtractor::new(),
                output_dir: output_dir.clone(),
                initialized: false,
            });
        }

        // Create VideoToolbox compression session
        println!("Creating H.264 compression session...");
        let compression_session = match create_compression_session() {
            Ok(s) => s,
            Err(e) => {
                eprintln!("Failed to create compression session: OSStatus {}", e);
                return;
            }
        };
        println!("Compression session created successfully!");

        // Store compression session globally for delegate access
        COMPRESSION_SESSION = compression_session;

        // Set up AVCaptureSession
        println!("Setting up camera capture...");

        let capture_session = AVCaptureSession::new();
        capture_session.beginConfiguration();

        // Set session preset for 720p
        let preset = ns_string!("AVCaptureSessionPreset1280x720");
        let can_set: Bool = msg_send![&capture_session, canSetSessionPreset: preset];
        if can_set.as_bool() {
            let _: () = msg_send![&capture_session, setSessionPreset: preset];
        }

        // Get default video device
        let media_type = AVMediaTypeVideo.expect("AVMediaTypeVideo not available");
        let video_device = AVCaptureDevice::defaultDeviceWithMediaType(media_type);
        let video_device = match video_device {
            Some(d) => d,
            None => {
                eprintln!("No camera device found!");
                return;
            }
        };

        println!("Using camera: {:?}", video_device.localizedName());

        // Create device input
        let device_input_result = AVCaptureDeviceInput::deviceInputWithDevice_error(&video_device);

        let device_input = match device_input_result {
            Ok(i) => i,
            Err(e) => {
                eprintln!("Failed to create device input: {:?}", e);
                return;
            }
        };

        // Add input to session
        if capture_session.canAddInput(&device_input) {
            capture_session.addInput(&device_input);
        } else {
            eprintln!("Cannot add camera input to session");
            return;
        }

        // Create video data output
        let video_output = AVCaptureVideoDataOutput::new();

        // Set pixel format to BGRA
        let format_key = ns_string!("PixelFormatType");
        let format_value: Retained<NSNumber> =
            msg_send![class!(NSNumber), numberWithUnsignedInt: codecs::pixel::BGRA32];

        let video_settings: Retained<NSObject> = msg_send![
            class!(NSDictionary),
            dictionaryWithObject: &*format_value,
            forKey: format_key
        ];

        let _: () = msg_send![&video_output, setVideoSettings: &*video_settings];
        video_output.setAlwaysDiscardsLateVideoFrames(true);

        // Create delegate
        let delegate = create_capture_delegate(
            "FMP4CameraDelegate",
            "AVCaptureVideoDataOutputSampleBufferDelegate",
            capture_output_did_output as DelegateCallback,
        )
        .expect("Failed to create delegate");

        // Create dispatch queue
        let callback_queue = create_dispatch_queue("com.videotoolbox.fmp4.queue");

        // Set delegate
        set_sample_buffer_delegate(
            &*video_output as *const _ as *const c_void,
            &*delegate as *const _ as *const c_void,
            callback_queue,
        );

        // Add output to session
        if capture_session.canAddOutput(&video_output) {
            capture_session.addOutput(&video_output);
        } else {
            eprintln!("Cannot add video output to session");
            return;
        }

        // Commit configuration
        capture_session.commitConfiguration();

        // Start recording
        println!("\nStarting camera capture...");
        println!("Recording for {} seconds...\n", RECORD_DURATION_SECS);

        capture_session.startRunning();

        // Keep delegate alive
        let _delegate_ref = delegate.clone();

        // Run the run loop
        let mut last_printed: u64 = 0;
        run_for_duration(Duration::from_secs(RECORD_DURATION_SECS), |elapsed| {
            let secs = elapsed.as_secs();
            if secs > last_printed {
                last_printed = secs;
                println!(
                    "  {} sec - {} frames captured, {} segments",
                    secs,
                    FRAME_COUNT.load(Ordering::SeqCst),
                    SEGMENT_COUNT.load(Ordering::SeqCst)
                );
            }
        });

        // Stop recording
        println!("\nStopping capture...");
        SHOULD_STOP.store(true, Ordering::SeqCst);

        capture_session.stopRunning();

        // Complete encoding
        let complete_time = CMTime {
            value: FRAME_COUNT.load(Ordering::SeqCst) as i64,
            timescale: FRAME_RATE as i32,
            flags: 1,
            epoch: 0,
        };
        VTCompressionSessionCompleteFrames(compression_session, complete_time);

        // Wait for final frames
        std::thread::sleep(Duration::from_millis(500));

        // Flush remaining frames
        {
            let mut ctx_guard = MUXER_CONTEXT.lock().unwrap();
            if let Some(ctx) = ctx_guard.as_mut() {
                if let Some(segment) = ctx.muxer.flush() {
                    let segment_num = SEGMENT_COUNT.fetch_add(1, Ordering::SeqCst) + 1;
                    let segment_path = ctx.output_dir.join(format!("segment_{:03}.m4s", segment_num));

                    if let Ok(mut file) = File::create(&segment_path) {
                        if file.write_all(&segment).is_ok() {
                            println!(
                                "  Created final segment {}: {} ({} bytes)",
                                segment_num,
                                segment_path.display(),
                                segment.len()
                            );
                        }
                    }
                }
            }
        }

        // Clean up compression session
        VTCompressionSessionInvalidate(compression_session);

        // Print summary
        let total_frames = FRAME_COUNT.load(Ordering::SeqCst);
        let encoded_frames = ENCODED_FRAMES.load(Ordering::SeqCst);
        let total_segments = SEGMENT_COUNT.load(Ordering::SeqCst);

        println!("\n=================================");
        println!("Recording complete!");
        println!("  Captured frames: {}", total_frames);
        println!("  Encoded frames: {}", encoded_frames);
        println!("  Segments created: {}", total_segments);
        println!("  Output directory: {}", output_dir.display());

        // Calculate total size
        let mut total_size = 0u64;
        if let Ok(entries) = fs::read_dir(&output_dir) {
            for entry in entries.flatten() {
                if let Ok(metadata) = entry.metadata() {
                    total_size += metadata.len();
                }
            }
        }
        println!("  Total size: {:.2} MB", total_size as f64 / (1024.0 * 1024.0));

        println!("\nTo play the output:");
        println!("  cat {}/init.mp4 {}/segment_*.m4s > combined.mp4", output_dir.display(), output_dir.display());
        println!("  ffplay combined.mp4");

        println!("\nDone!");
    }
}
