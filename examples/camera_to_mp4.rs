//! Capture video from camera using AVFoundation, compress with H.264, and save to MP4.
//!
//! This example demonstrates the full pipeline:
//! 1. AVCaptureSession to capture video from the default camera
//! 2. VTCompressionSession to encode frames as H.264
//! 3. AVAssetWriter to write the encoded video to an MP4 file
//!
//! Run with: cargo run --example camera_to_mp4
//!
//! Note: You may need to grant camera permissions when running.
//! The output file will be saved to the current directory as "output.mov".

use core_foundation::base::TCFType;
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::CFDictionary;
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;
use core_foundation_sys::base::{kCFAllocatorDefault, CFTypeRef, OSStatus};
use core_foundation_sys::dictionary::CFDictionaryRef;
use core_foundation_sys::string::CFStringRef;
use core_media_sys::CMTime;
use core_video_sys::{kCVPixelBufferPixelFormatTypeKey, CVPixelBufferRef};
use libc::c_void;
use objc2::rc::Retained;
use objc2::runtime::{AnyProtocol, Bool, Sel};
use objc2::{class, msg_send, sel, ClassType};
use objc2_av_foundation::{
    AVAssetWriter, AVAssetWriterInput, AVCaptureDevice, AVCaptureDeviceInput, AVCaptureSession,
    AVCaptureVideoDataOutput, AVMediaTypeVideo,
};
use objc2_core_media::CMSampleBuffer;
use objc2_foundation::{ns_string, NSError, NSNumber, NSObject, NSString, NSURL};
use std::ffi::CStr;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;
use video_toolbox_sys::compression::{
    kVTCompressionPropertyKey_AverageBitRate, kVTCompressionPropertyKey_ExpectedFrameRate,
    kVTCompressionPropertyKey_MaxKeyFrameInterval, kVTCompressionPropertyKey_ProfileLevel,
    kVTCompressionPropertyKey_RealTime, kVTEncodeInfo_FrameDropped,
    kVTProfileLevel_H264_High_AutoLevel,
    kVTVideoEncoderSpecification_EnableHardwareAcceleratedVideoEncoder,
    VTCompressionSessionCompleteFrames, VTCompressionSessionCreate,
    VTCompressionSessionEncodeFrame, VTCompressionSessionInvalidate,
    VTCompressionSessionPrepareToEncodeFrames, VTCompressionSessionRef, VTEncodeInfoFlags,
};
use video_toolbox_sys::session::VTSessionSetProperty;

// H.264 codec FourCC: 'avc1'
const K_CM_VIDEO_CODEC_TYPE_H264: u32 = 0x61766331;

// kCVPixelFormatType_32BGRA
const K_CV_PIXEL_FORMAT_TYPE_32BGRA: u32 = 0x42475241; // 'BGRA'

// Recording parameters
const WIDTH: i32 = 1280;
const HEIGHT: i32 = 720;
const FRAME_RATE: f64 = 30.0;
const BITRATE: i64 = 4_000_000; // 4 Mbps
const RECORD_DURATION_SECS: u64 = 5;

// Global state for the encoding pipeline
static FRAME_COUNT: AtomicUsize = AtomicUsize::new(0);
static ENCODED_FRAMES: AtomicUsize = AtomicUsize::new(0);
static SHOULD_STOP: AtomicBool = AtomicBool::new(false);

// Thread-safe wrapper for the asset writer input
#[allow(dead_code)]
struct EncoderContext {
    asset_writer: Retained<AVAssetWriter>,
    writer_input: Retained<AVAssetWriterInput>,
}

unsafe impl Send for EncoderContext {}
unsafe impl Sync for EncoderContext {}

static ENCODER_CONTEXT: Mutex<Option<EncoderContext>> = Mutex::new(None);

// Global compression session (needed for the delegate callback)
static mut COMPRESSION_SESSION: VTCompressionSessionRef = ptr::null_mut();

// CoreMedia FFI
#[link(name = "CoreMedia", kind = "framework")]
extern "C" {
    fn CMSampleBufferGetImageBuffer(sbuf: *const c_void) -> CVPixelBufferRef;
    fn CMSampleBufferGetTotalSampleSize(sbuf: *const c_void) -> usize;
}

// Dispatch FFI
#[link(name = "System")]
extern "C" {
    fn dispatch_queue_create(label: *const i8, attr: *const c_void) -> *mut c_void;
}

// CoreFoundation run loop FFI
#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFRunLoopGetMain() -> *mut c_void;
    fn CFRunLoopRunInMode(mode: *const c_void, seconds: f64, return_after_source_handled: bool) -> i32;
    static kCFRunLoopDefaultMode: *const c_void;
}

// Type alias for VTCompressionOutputCallback
type CompressionCallback = extern "C" fn(
    output_callback_ref_con: *mut c_void,
    source_frame_ref_con: *mut c_void,
    status: OSStatus,
    info_flags: VTEncodeInfoFlags,
    sample_buffer: *mut c_void,
);

// Callback invoked when VideoToolbox has an encoded frame ready
extern "C" fn compression_output_callback(
    _output_callback_ref_con: *mut c_void,
    _source_frame_ref_con: *mut c_void,
    status: OSStatus,
    info_flags: VTEncodeInfoFlags,
    sample_buffer: *mut c_void,
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

    // Get the size of the encoded data (for stats)
    let _data_size = unsafe { CMSampleBufferGetTotalSampleSize(sample_buffer) };

    // Append the encoded sample buffer to the asset writer
    let ctx_guard = ENCODER_CONTEXT.lock().unwrap();
    if let Some(ref ctx) = *ctx_guard {
        unsafe {
            // Convert raw pointer to objc2 reference
            let sample_buffer_obj: &CMSampleBuffer = &*(sample_buffer as *const CMSampleBuffer);

            if ctx.writer_input.isReadyForMoreMediaData() {
                let success: Bool =
                    msg_send![&ctx.writer_input, appendSampleBuffer: sample_buffer_obj];
                if success.as_bool() {
                    let frame_num = ENCODED_FRAMES.fetch_add(1, Ordering::SeqCst) + 1;
                    if frame_num % 30 == 0 {
                        println!("  Encoded {} frames...", frame_num);
                    }
                } else {
                    eprintln!("Failed to append sample buffer to asset writer");
                }
            }
        }
    }
}

fn create_compression_session() -> Result<VTCompressionSessionRef, OSStatus> {
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
        let width_key = CFString::from_static_string("Width");
        let height_key = CFString::from_static_string("Height");

        let source_attrs = CFDictionary::from_CFType_pairs(&[
            (
                format_key.as_CFType(),
                CFNumber::from(K_CV_PIXEL_FORMAT_TYPE_32BGRA as i32).as_CFType(),
            ),
            (width_key.as_CFType(), CFNumber::from(WIDTH).as_CFType()),
            (height_key.as_CFType(), CFNumber::from(HEIGHT).as_CFType()),
        ]);

        let mut session: VTCompressionSessionRef = ptr::null_mut();

        let status = VTCompressionSessionCreate(
            kCFAllocatorDefault,
            WIDTH,
            HEIGHT,
            K_CM_VIDEO_CODEC_TYPE_H264,
            encoder_spec.as_concrete_TypeRef() as CFDictionaryRef,
            source_attrs.as_concrete_TypeRef() as CFDictionaryRef,
            kCFAllocatorDefault,
            std::mem::transmute::<CompressionCallback, _>(compression_output_callback),
            ptr::null_mut(),
            &mut session,
        );

        if status != 0 {
            return Err(status);
        }

        // Configure session properties
        // Profile: H.264 High (auto level)
        let profile_key =
            CFString::wrap_under_get_rule(kVTCompressionPropertyKey_ProfileLevel as CFStringRef);
        let profile_value =
            CFString::wrap_under_get_rule(kVTProfileLevel_H264_High_AutoLevel as CFStringRef);
        VTSessionSetProperty(
            session,
            profile_key.as_concrete_TypeRef() as CFStringRef,
            profile_value.as_concrete_TypeRef() as CFTypeRef,
        );

        // Bitrate
        let bitrate_key =
            CFString::wrap_under_get_rule(kVTCompressionPropertyKey_AverageBitRate as CFStringRef);
        let bitrate_value = CFNumber::from(BITRATE);
        VTSessionSetProperty(
            session,
            bitrate_key.as_concrete_TypeRef() as CFStringRef,
            bitrate_value.as_concrete_TypeRef() as CFTypeRef,
        );

        // Expected frame rate
        let fps_key = CFString::wrap_under_get_rule(
            kVTCompressionPropertyKey_ExpectedFrameRate as CFStringRef,
        );
        let fps_value = CFNumber::from(FRAME_RATE);
        VTSessionSetProperty(
            session,
            fps_key.as_concrete_TypeRef() as CFStringRef,
            fps_value.as_concrete_TypeRef() as CFTypeRef,
        );

        // Keyframe interval
        let keyframe_key = CFString::wrap_under_get_rule(
            kVTCompressionPropertyKey_MaxKeyFrameInterval as CFStringRef,
        );
        let keyframe_value = CFNumber::from(FRAME_RATE as i32);
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

        // Prepare for encoding
        let prep_status = VTCompressionSessionPrepareToEncodeFrames(session);
        if prep_status != 0 {
            VTCompressionSessionInvalidate(session);
            return Err(prep_status);
        }

        Ok(session)
    }
}

fn setup_asset_writer(
    output_path: &str,
) -> Result<(Retained<AVAssetWriter>, Retained<AVAssetWriterInput>), String> {
    unsafe {
        // Create output URL
        let path_str = NSString::from_str(output_path);
        let output_url = NSURL::fileURLWithPath(&path_str);

        // Remove existing file if present
        let file_manager: Retained<NSObject> = msg_send![class!(NSFileManager), defaultManager];
        let _: Bool = msg_send![&file_manager, removeItemAtPath: &*path_str, error: ptr::null_mut::<*mut NSError>()];

        // Create asset writer for MOV/MP4
        let file_type = ns_string!("com.apple.quicktime-movie");

        let asset_writer_result =
            AVAssetWriter::assetWriterWithURL_fileType_error(&output_url, file_type);

        let asset_writer = match asset_writer_result {
            Ok(w) => w,
            Err(e) => return Err(format!("Failed to create asset writer: {:?}", e)),
        };

        // Create asset writer input for video
        // Use passthrough (nil settings) since we're providing already-encoded H.264 data
        let media_type = ns_string!("vide");
        let writer_input = AVAssetWriterInput::assetWriterInputWithMediaType_outputSettings(
            media_type,
            None, // passthrough mode - data is already H.264 encoded
        );

        writer_input.setExpectsMediaDataInRealTime(true);

        // Add input to writer
        asset_writer.addInput(&writer_input);

        // Start writing
        let success = asset_writer.startWriting();
        if !success {
            return Err("Failed to start asset writer".to_string());
        }

        // Start session at time zero
        let zero_time = objc2_core_media::CMTime {
            value: 0,
            timescale: 600,
            flags: objc2_core_media::CMTimeFlags(1),
            epoch: 0,
        };
        let _: () = msg_send![&asset_writer, startSessionAtSourceTime: zero_time];

        Ok((asset_writer, writer_input))
    }
}

// Delegate method for video frame capture
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
    println!("Camera to MP4 Recording Example");
    println!("================================");
    println!("Resolution: {}x{}", WIDTH, HEIGHT);
    println!("Frame rate: {} fps", FRAME_RATE);
    println!("Bitrate: {} Mbps", BITRATE / 1_000_000);
    println!("Duration: {} seconds\n", RECORD_DURATION_SECS);

    // Output file path
    let output_path = std::env::current_dir()
        .unwrap()
        .join("output.mov")
        .to_string_lossy()
        .to_string();

    println!("Output file: {}\n", output_path);

    unsafe {
        // 1. Set up AVAssetWriter for MP4 output
        println!("Setting up asset writer...");
        let (asset_writer, writer_input) = match setup_asset_writer(&output_path) {
            Ok((w, i)) => (w, i),
            Err(e) => {
                eprintln!("Failed to set up asset writer: {}", e);
                return;
            }
        };

        // 2. Create VideoToolbox compression session
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

        // Store encoder context for the callback
        {
            let mut ctx = ENCODER_CONTEXT.lock().unwrap();
            *ctx = Some(EncoderContext {
                asset_writer: asset_writer.clone(),
                writer_input: writer_input.clone(),
            });
        }

        // 3. Set up AVCaptureSession
        println!("Setting up camera capture...");

        let capture_session = AVCaptureSession::new();

        // Begin configuration
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
            msg_send![class!(NSNumber), numberWithUnsignedInt: K_CV_PIXEL_FORMAT_TYPE_32BGRA];

        let video_settings: Retained<NSObject> = msg_send![
            class!(NSDictionary),
            dictionaryWithObject: &*format_value,
            forKey: format_key
        ];

        let _: () = msg_send![&video_output, setVideoSettings: &*video_settings];
        video_output.setAlwaysDiscardsLateVideoFrames(true);

        // Build delegate class dynamically
        let protocol_name =
            CStr::from_bytes_with_nul(b"AVCaptureVideoDataOutputSampleBufferDelegate\0").unwrap();
        let delegate_protocol = AnyProtocol::get(protocol_name).expect("Protocol not found");

        use objc2::declare::ClassBuilder;

        // Create a unique class name to avoid conflicts
        let class_name = CStr::from_bytes_with_nul(b"CameraDelegate\0").unwrap();
        let mut builder = ClassBuilder::new(class_name, NSObject::class()).unwrap();
        builder.add_protocol(delegate_protocol);

        // Register the class first
        let delegate_class = builder.register();

        // Add the delegate method after registration using class_addMethod
        #[link(name = "objc", kind = "dylib")]
        extern "C" {
            fn class_addMethod(
                cls: *const c_void,
                name: Sel,
                imp: *const c_void,
                types: *const i8,
            ) -> Bool;
        }

        let method_sel = sel!(captureOutput:didOutputSampleBuffer:fromConnection:);

        // Method signature: v@:@@@ (void, self, _cmd, output, sampleBuffer, connection)
        let method_types = b"v@:@@@\0";
        let added = class_addMethod(
            delegate_class as *const _ as *const c_void,
            method_sel,
            capture_output_did_output as *const c_void,
            method_types.as_ptr() as *const i8,
        );
        println!("  Method added to class: {}", added.as_bool());

        // Create delegate instance
        let delegate: Retained<NSObject> = msg_send![delegate_class, new];

        // Create dispatch queue
        let queue_label = b"com.videotoolbox.camera.queue\0";
        let callback_queue =
            dispatch_queue_create(queue_label.as_ptr() as *const i8, ptr::null());

        // Set delegate using properly typed objc_msgSend to avoid ARM64 variadic calling convention issues
        #[link(name = "objc", kind = "dylib")]
        extern "C" {
            // Use a typed function pointer instead of variadic to avoid ARM64 ABI issues
            #[link_name = "objc_msgSend"]
            fn objc_msgSend_set_delegate(
                receiver: *const c_void,
                sel: Sel,
                delegate: *const c_void,
                queue: *const c_void,
            );
        }

        let set_delegate_sel = sel!(setSampleBufferDelegate:queue:);
        objc_msgSend_set_delegate(
            &*video_output as *const _ as *const c_void,
            set_delegate_sel,
            &*delegate as *const _ as *const c_void,
            callback_queue,
        );

        // Verify delegate was set using raw objc_msgSend
        #[link(name = "objc", kind = "dylib")]
        extern "C" {
            #[link_name = "objc_msgSend"]
            fn objc_msgSend_ptr(receiver: *mut c_void, sel: Sel) -> *mut c_void;
            #[link_name = "objc_msgSend"]
            fn objc_msgSend_bool(receiver: *mut c_void, sel: Sel, arg: Sel) -> Bool;
        }

        let current_delegate = objc_msgSend_ptr(
            &*video_output as *const _ as *mut c_void,
            sel!(sampleBufferDelegate),
        );
        println!("  Delegate set: {}", !current_delegate.is_null());

        // Check if our class responds to the selector
        let responds = objc_msgSend_bool(
            &*delegate as *const _ as *mut c_void,
            sel!(respondsToSelector:),
            sel!(captureOutput:didOutputSampleBuffer:fromConnection:),
        );
        println!("  Delegate responds to selector: {}", responds.as_bool());

        // Add output to session
        if capture_session.canAddOutput(&video_output) {
            capture_session.addOutput(&video_output);
        } else {
            eprintln!("Cannot add video output to session");
            return;
        }

        // Commit configuration
        capture_session.commitConfiguration();

        // 4. Start recording
        println!("\nStarting camera capture...");
        println!("Recording for {} seconds...\n", RECORD_DURATION_SECS);

        capture_session.startRunning();

        // Verify capture session is running
        let is_running: Bool = msg_send![&capture_session, isRunning];
        println!("  Capture session running: {}", is_running.as_bool());

        // Keep delegate alive by storing it
        let _delegate_ref = delegate.clone();

        // Run the run loop to process callbacks
        // AVFoundation needs a run loop to deliver sample buffer callbacks
        let start = std::time::Instant::now();
        while start.elapsed() < Duration::from_secs(RECORD_DURATION_SECS) {
            // Run the run loop for a short interval to process callbacks
            CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.1, false);

            // Print progress every second
            let elapsed = start.elapsed().as_secs();
            static mut LAST_PRINTED: u64 = 0;
            if elapsed > LAST_PRINTED {
                LAST_PRINTED = elapsed;
                println!("  {} sec - {} frames captured", elapsed, FRAME_COUNT.load(Ordering::SeqCst));
            }
        }

        // 5. Stop recording
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

        // Wait a moment for final frames to be encoded
        std::thread::sleep(Duration::from_millis(500));

        // Mark input as finished and finish writing
        writer_input.markAsFinished();

        // Finish writing asynchronously
        println!("Finalizing video file...");

        // Use block2 for completion handler
        use block2::StackBlock;

        let finished = std::sync::Arc::new(AtomicBool::new(false));
        let finished_clone = finished.clone();

        let block = StackBlock::new(move || {
            finished_clone.store(true, Ordering::SeqCst);
        });

        let _: () = msg_send![&asset_writer, finishWritingWithCompletionHandler: &block];

        // Wait for writing to finish (with timeout)
        let timeout = std::time::Instant::now();
        while !finished.load(Ordering::SeqCst) {
            if timeout.elapsed() > Duration::from_secs(10) {
                eprintln!("Timeout waiting for asset writer to finish");
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }

        // Clean up compression session
        VTCompressionSessionInvalidate(compression_session);

        // Print summary
        let total_frames = FRAME_COUNT.load(Ordering::SeqCst);
        let encoded_frames = ENCODED_FRAMES.load(Ordering::SeqCst);

        println!("\n================================");
        println!("Recording complete!");
        println!("  Captured frames: {}", total_frames);
        println!("  Encoded frames: {}", encoded_frames);
        println!("  Output file: {}", output_path);

        // Check file size
        if let Ok(metadata) = std::fs::metadata(&output_path) {
            println!(
                "  File size: {:.2} MB",
                metadata.len() as f64 / (1024.0 * 1024.0)
            );
        }

        println!("\nDone!");
    }
}
