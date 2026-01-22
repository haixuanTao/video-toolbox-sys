//! Capture audio and video simultaneously, encode to H.264 + AAC, save to MOV.
//!
//! This example demonstrates a complete A/V recording pipeline:
//! 1. AVCaptureSession capturing from both camera and microphone
//! 2. VTCompressionSession for H.264 video encoding
//! 3. AVAssetWriter for muxing video + audio into a MOV file
//!
//! Run with: cargo run --example av_record --features helpers
//!
//! Note: You may need to grant camera and microphone permissions.
//! The output file will be saved as "output_av.mov".

use core_foundation_sys::base::OSStatus;
use core_media_sys::CMTime;
use libc::c_void;
use objc2::rc::Retained;
use objc2::runtime::{Bool, Sel};
use objc2::{class, msg_send};
use objc2_av_foundation::{
    AVAssetWriter, AVAssetWriterInput, AVCaptureAudioDataOutput, AVCaptureDevice,
    AVCaptureDeviceInput, AVCaptureSession, AVCaptureVideoDataOutput, AVMediaTypeAudio,
    AVMediaTypeVideo,
};
use objc2_core_media::CMSampleBuffer;
use objc2_foundation::{ns_string, NSDictionary, NSError, NSNumber, NSObject, NSString, NSURL};
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
    CompressionSessionBuilder, DelegateCallback,
};

// Video parameters
const VIDEO_WIDTH: i32 = 1280;
const VIDEO_HEIGHT: i32 = 720;
const FRAME_RATE: f64 = 30.0;
const VIDEO_BITRATE: i64 = 4_000_000; // 4 Mbps

// Audio parameters
const SAMPLE_RATE: f64 = 44100.0;
const NUM_CHANNELS: u32 = 1;
const AUDIO_BITRATE: i32 = 128000; // 128 kbps

// Recording duration
const RECORD_DURATION_SECS: u64 = 5;

// Global state
static VIDEO_FRAME_COUNT: AtomicUsize = AtomicUsize::new(0);
static ENCODED_VIDEO_FRAMES: AtomicUsize = AtomicUsize::new(0);
static AUDIO_SAMPLE_COUNT: AtomicUsize = AtomicUsize::new(0);
static SHOULD_STOP: AtomicBool = AtomicBool::new(false);

// Writer context for both video and audio
#[allow(dead_code)]
struct AVWriterContext {
    asset_writer: Retained<AVAssetWriter>,
    video_input: Retained<AVAssetWriterInput>,
    audio_input: Retained<AVAssetWriterInput>,
}

unsafe impl Send for AVWriterContext {}
unsafe impl Sync for AVWriterContext {}

static WRITER_CONTEXT: Mutex<Option<AVWriterContext>> = Mutex::new(None);
static mut COMPRESSION_SESSION: VTCompressionSessionRef = ptr::null_mut();

// CoreMedia FFI
#[link(name = "CoreMedia", kind = "framework")]
extern "C" {
    fn CMSampleBufferGetImageBuffer(sbuf: *const c_void) -> CVPixelBufferRef;
}

// Video compression output callback
extern "C" fn compression_output_callback(
    _output_callback_ref_con: *mut c_void,
    _source_frame_ref_con: *mut c_void,
    status: OSStatus,
    info_flags: VTEncodeInfoFlags,
    sample_buffer: *mut c_void,
) {
    if status != 0 || sample_buffer.is_null() {
        return;
    }

    if (info_flags & kVTEncodeInfo_FrameDropped) != 0 {
        return;
    }

    let ctx_guard = WRITER_CONTEXT.lock().unwrap();
    if let Some(ref ctx) = *ctx_guard {
        unsafe {
            let sample_buffer_obj: &CMSampleBuffer = &*(sample_buffer as *const CMSampleBuffer);

            if ctx.video_input.isReadyForMoreMediaData() {
                let success: Bool =
                    msg_send![&ctx.video_input, appendSampleBuffer: sample_buffer_obj];
                if success.as_bool() {
                    ENCODED_VIDEO_FRAMES.fetch_add(1, Ordering::SeqCst);
                }
            }
        }
    }
}

// Video capture delegate callback
extern "C" fn video_capture_callback(
    _this: *mut c_void,
    _cmd: Sel,
    _output: *mut c_void,
    sample_buffer: *mut c_void,
    _connection: *mut c_void,
) {
    unsafe {
        if SHOULD_STOP.load(Ordering::SeqCst) || sample_buffer.is_null() {
            return;
        }

        let pixel_buffer = CMSampleBufferGetImageBuffer(sample_buffer);
        if pixel_buffer.is_null() {
            return;
        }

        let frame_num = VIDEO_FRAME_COUNT.fetch_add(1, Ordering::SeqCst);

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

        let mut info_flags: VTEncodeInfoFlags = 0;
        VTCompressionSessionEncodeFrame(
            COMPRESSION_SESSION,
            pixel_buffer,
            pts,
            duration,
            ptr::null(),
            ptr::null_mut(),
            &mut info_flags,
        );
    }
}

// Audio capture delegate callback
extern "C" fn audio_capture_callback(
    _this: *mut c_void,
    _cmd: Sel,
    _output: *mut c_void,
    sample_buffer: *mut c_void,
    _connection: *mut c_void,
) {
    unsafe {
        if SHOULD_STOP.load(Ordering::SeqCst) || sample_buffer.is_null() {
            return;
        }

        AUDIO_SAMPLE_COUNT.fetch_add(1, Ordering::SeqCst);

        let ctx_guard = WRITER_CONTEXT.lock().unwrap();
        if let Some(ref ctx) = *ctx_guard {
            let sample_buffer_obj: &CMSampleBuffer = &*(sample_buffer as *const CMSampleBuffer);

            if ctx.audio_input.isReadyForMoreMediaData() {
                let _: Bool = msg_send![&ctx.audio_input, appendSampleBuffer: sample_buffer_obj];
            }
        }
    }
}

fn create_compression_session() -> Result<VTCompressionSessionRef, OSStatus> {
    unsafe {
        CompressionSessionBuilder::new(VIDEO_WIDTH, VIDEO_HEIGHT, codecs::video::H264)
            .pixel_format(codecs::pixel::BGRA32)
            .hardware_accelerated(true)
            .bitrate(VIDEO_BITRATE)
            .frame_rate(FRAME_RATE)
            .keyframe_interval(FRAME_RATE as i32)
            .real_time(true)
            .profile_level(kVTProfileLevel_H264_High_AutoLevel)
            .build_with_context(Some(compression_output_callback), ptr::null_mut())
    }
}

fn setup_asset_writer(
    output_path: &str,
) -> Result<
    (
        Retained<AVAssetWriter>,
        Retained<AVAssetWriterInput>,
        Retained<AVAssetWriterInput>,
    ),
    String,
> {
    unsafe {
        let path_str = NSString::from_str(output_path);
        let output_url = NSURL::fileURLWithPath(&path_str);

        // Remove existing file
        let file_manager: Retained<NSObject> = msg_send![class!(NSFileManager), defaultManager];
        let _: Bool = msg_send![&file_manager, removeItemAtPath: &*path_str, error: ptr::null_mut::<*mut NSError>()];

        let file_type = ns_string!("com.apple.quicktime-movie");
        let asset_writer = AVAssetWriter::assetWriterWithURL_fileType_error(&output_url, file_type)
            .map_err(|e| format!("Failed to create asset writer: {:?}", e))?;

        // Video input (passthrough for H.264)
        let video_media_type = ns_string!("vide");
        let video_input = AVAssetWriterInput::assetWriterInputWithMediaType_outputSettings(
            video_media_type,
            None,
        );
        video_input.setExpectsMediaDataInRealTime(true);

        // Audio input with AAC encoding settings
        let audio_media_type = ns_string!("soun");

        let format_key = NSString::from_str("AVFormatIDKey");
        let sample_rate_key = NSString::from_str("AVSampleRateKey");
        let channels_key = NSString::from_str("AVNumberOfChannelsKey");
        let bitrate_key = NSString::from_str("AVEncoderBitRateKey");

        let format_value: Retained<NSNumber> =
            msg_send![class!(NSNumber), numberWithUnsignedInt: codecs::audio::AAC];
        let sample_rate_value: Retained<NSNumber> =
            msg_send![class!(NSNumber), numberWithDouble: SAMPLE_RATE];
        let channels_value: Retained<NSNumber> =
            msg_send![class!(NSNumber), numberWithUnsignedInt: NUM_CHANNELS];
        let bitrate_value: Retained<NSNumber> =
            msg_send![class!(NSNumber), numberWithInt: AUDIO_BITRATE];

        let keys: [&NSString; 4] = [&format_key, &sample_rate_key, &channels_key, &bitrate_key];
        let objects: [&NSNumber; 4] = [
            &format_value,
            &sample_rate_value,
            &channels_value,
            &bitrate_value,
        ];

        let audio_settings: Retained<NSDictionary<NSString, NSObject>> = msg_send![
            class!(NSDictionary),
            dictionaryWithObjects: objects.as_ptr(),
            forKeys: keys.as_ptr(),
            count: 4usize
        ];

        let audio_input: Retained<AVAssetWriterInput> = msg_send![
            class!(AVAssetWriterInput),
            assetWriterInputWithMediaType: audio_media_type,
            outputSettings: &*audio_settings
        ];
        audio_input.setExpectsMediaDataInRealTime(true);

        // Add inputs
        asset_writer.addInput(&video_input);
        asset_writer.addInput(&audio_input);

        if !asset_writer.startWriting() {
            return Err("Failed to start asset writer".to_string());
        }

        let zero_time = objc2_core_media::CMTime {
            value: 0,
            timescale: 600,
            flags: objc2_core_media::CMTimeFlags(1),
            epoch: 0,
        };
        let _: () = msg_send![&asset_writer, startSessionAtSourceTime: zero_time];

        Ok((asset_writer, video_input, audio_input))
    }
}

fn main() {
    println!("Audio + Video Recording Example");
    println!("================================");
    println!(
        "Video: {}x{} @ {} fps, H.264 {} Mbps",
        VIDEO_WIDTH,
        VIDEO_HEIGHT,
        FRAME_RATE,
        VIDEO_BITRATE / 1_000_000
    );
    println!(
        "Audio: {} Hz, {} ch, AAC {} kbps",
        SAMPLE_RATE,
        NUM_CHANNELS,
        AUDIO_BITRATE / 1000
    );
    println!("Duration: {} seconds\n", RECORD_DURATION_SECS);

    let output_path = std::env::current_dir()
        .unwrap()
        .join("output_av.mov")
        .to_string_lossy()
        .to_string();

    println!("Output file: {}\n", output_path);

    unsafe {
        // Set up asset writer
        println!("Setting up asset writer...");
        let (asset_writer, video_input, audio_input) = match setup_asset_writer(&output_path) {
            Ok(x) => x,
            Err(e) => {
                eprintln!("Failed: {}", e);
                return;
            }
        };

        // Create video compression session using builder
        println!("Creating H.264 encoder...");
        let compression_session = match create_compression_session() {
            Ok(s) => s,
            Err(e) => {
                eprintln!("Failed to create compression session: {}", e);
                return;
            }
        };
        COMPRESSION_SESSION = compression_session;

        // Store writer context
        {
            let mut ctx = WRITER_CONTEXT.lock().unwrap();
            *ctx = Some(AVWriterContext {
                asset_writer: asset_writer.clone(),
                video_input: video_input.clone(),
                audio_input: audio_input.clone(),
            });
        }

        // Set up capture session
        println!("Setting up capture session...");
        let capture_session = AVCaptureSession::new();
        capture_session.beginConfiguration();

        // Set preset
        let preset = ns_string!("AVCaptureSessionPreset1280x720");
        let can_set: Bool = msg_send![&capture_session, canSetSessionPreset: preset];
        if can_set.as_bool() {
            let _: () = msg_send![&capture_session, setSessionPreset: preset];
        }

        // Add video device
        let video_media = AVMediaTypeVideo.expect("AVMediaTypeVideo not available");
        let video_device = AVCaptureDevice::defaultDeviceWithMediaType(video_media);
        let video_device = match video_device {
            Some(d) => d,
            None => {
                eprintln!("No camera found!");
                return;
            }
        };
        println!("  Camera: {:?}", video_device.localizedName());

        let video_device_input =
            AVCaptureDeviceInput::deviceInputWithDevice_error(&video_device).unwrap();
        if capture_session.canAddInput(&video_device_input) {
            capture_session.addInput(&video_device_input);
        }

        // Add audio device
        let audio_media = AVMediaTypeAudio.expect("AVMediaTypeAudio not available");
        let audio_device = AVCaptureDevice::defaultDeviceWithMediaType(audio_media);
        let audio_device = match audio_device {
            Some(d) => d,
            None => {
                eprintln!("No microphone found!");
                return;
            }
        };
        println!("  Microphone: {:?}", audio_device.localizedName());

        let audio_device_input =
            AVCaptureDeviceInput::deviceInputWithDevice_error(&audio_device).unwrap();
        if capture_session.canAddInput(&audio_device_input) {
            capture_session.addInput(&audio_device_input);
        }

        // Create video output
        let video_output = AVCaptureVideoDataOutput::new();
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

        // Create audio output
        let audio_output = AVCaptureAudioDataOutput::new();

        // Create delegates using library helpers
        let video_delegate = create_capture_delegate(
            "VideoDelegate",
            "AVCaptureVideoDataOutputSampleBufferDelegate",
            video_capture_callback as DelegateCallback,
        )
        .expect("Failed to create video delegate");

        let audio_delegate = create_capture_delegate(
            "AudioDelegate",
            "AVCaptureAudioDataOutputSampleBufferDelegate",
            audio_capture_callback as DelegateCallback,
        )
        .expect("Failed to create audio delegate");

        // Create dispatch queues using library helper
        let video_queue = create_dispatch_queue("com.av.video.queue");
        let audio_queue = create_dispatch_queue("com.av.audio.queue");

        // Set delegates using library helper
        set_sample_buffer_delegate(
            &*video_output as *const _ as *const c_void,
            &*video_delegate as *const _ as *const c_void,
            video_queue,
        );
        set_sample_buffer_delegate(
            &*audio_output as *const _ as *const c_void,
            &*audio_delegate as *const _ as *const c_void,
            audio_queue,
        );

        // Add outputs
        if capture_session.canAddOutput(&video_output) {
            capture_session.addOutput(&video_output);
        }
        if capture_session.canAddOutput(&audio_output) {
            capture_session.addOutput(&audio_output);
        }

        capture_session.commitConfiguration();

        // Start recording
        println!("\nStarting recording...");
        println!("Recording for {} seconds...\n", RECORD_DURATION_SECS);

        capture_session.startRunning();

        // Keep delegates alive
        let _video_delegate_ref = video_delegate.clone();
        let _audio_delegate_ref = audio_delegate.clone();

        // Run loop using library helper
        let mut last_printed: u64 = 0;
        run_for_duration(Duration::from_secs(RECORD_DURATION_SECS), |elapsed| {
            let secs = elapsed.as_secs();
            if secs > last_printed {
                last_printed = secs;
                println!(
                    "  {} sec - video: {} frames, audio: {} samples",
                    secs,
                    VIDEO_FRAME_COUNT.load(Ordering::SeqCst),
                    AUDIO_SAMPLE_COUNT.load(Ordering::SeqCst)
                );
            }
        });

        // Stop
        println!("\nStopping...");
        SHOULD_STOP.store(true, Ordering::SeqCst);
        capture_session.stopRunning();

        // Complete video encoding
        let complete_time = CMTime {
            value: VIDEO_FRAME_COUNT.load(Ordering::SeqCst) as i64,
            timescale: FRAME_RATE as i32,
            flags: 1,
            epoch: 0,
        };
        VTCompressionSessionCompleteFrames(compression_session, complete_time);
        std::thread::sleep(Duration::from_millis(500));

        // Finish writing
        video_input.markAsFinished();
        audio_input.markAsFinished();

        println!("Finalizing file...");

        use block2::StackBlock;
        let finished = std::sync::Arc::new(AtomicBool::new(false));
        let finished_clone = finished.clone();
        let block = StackBlock::new(move || {
            finished_clone.store(true, Ordering::SeqCst);
        });
        let _: () = msg_send![&asset_writer, finishWritingWithCompletionHandler: &block];

        let timeout = std::time::Instant::now();
        while !finished.load(Ordering::SeqCst) && timeout.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(100));
        }

        VTCompressionSessionInvalidate(compression_session);

        // Summary
        println!("\n================================");
        println!("Recording complete!");
        println!(
            "  Video frames captured: {}",
            VIDEO_FRAME_COUNT.load(Ordering::SeqCst)
        );
        println!(
            "  Video frames encoded: {}",
            ENCODED_VIDEO_FRAMES.load(Ordering::SeqCst)
        );
        println!(
            "  Audio samples: {}",
            AUDIO_SAMPLE_COUNT.load(Ordering::SeqCst)
        );
        println!("  Output: {}", output_path);

        if let Ok(metadata) = std::fs::metadata(&output_path) {
            println!(
                "  File size: {:.2} MB",
                metadata.len() as f64 / (1024.0 * 1024.0)
            );
        }

        println!("\nDone!");
    }
}
