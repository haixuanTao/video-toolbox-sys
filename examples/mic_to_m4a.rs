//! Capture audio from microphone using AVFoundation and save to M4A (AAC).
//!
//! This example demonstrates audio capture pipeline:
//! 1. AVCaptureSession to capture audio from the default microphone
//! 2. AVAssetWriter to encode and write AAC audio to an M4A file
//!
//! Run with: cargo run --example mic_to_m4a
//!
//! Note: You may need to grant microphone permissions when running.
//! The output file will be saved to the current directory as "output.m4a".

use libc::c_void;
use objc2::rc::Retained;
use objc2::runtime::{AnyProtocol, Bool, Sel};
use objc2::{class, msg_send, sel, ClassType};
use objc2_av_foundation::{
    AVAssetWriter, AVAssetWriterInput, AVCaptureDevice, AVCaptureDeviceInput, AVCaptureSession,
    AVCaptureAudioDataOutput, AVMediaTypeAudio,
};
use objc2_core_media::CMSampleBuffer;
use objc2_foundation::{ns_string, NSError, NSNumber, NSObject, NSString, NSURL, NSDictionary};
use std::ffi::CStr;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;

// Recording parameters
const SAMPLE_RATE: f64 = 44100.0;
const NUM_CHANNELS: u32 = 1; // Mono
const RECORD_DURATION_SECS: u64 = 5;

// Audio format constants
const K_AUDIO_FORMAT_MPEG4_AAC: u32 = 0x61616320; // 'aac '

// AVFoundation audio settings keys (as string constants)
const AV_FORMAT_ID_KEY: &str = "AVFormatIDKey";
const AV_SAMPLE_RATE_KEY: &str = "AVSampleRateKey";
const AV_NUMBER_OF_CHANNELS_KEY: &str = "AVNumberOfChannelsKey";
const AV_ENCODER_BIT_RATE_KEY: &str = "AVEncoderBitRateKey";

// Global state for the recording pipeline
static SAMPLE_COUNT: AtomicUsize = AtomicUsize::new(0);
static SHOULD_STOP: AtomicBool = AtomicBool::new(false);

// Thread-safe wrapper for the asset writer input
#[allow(dead_code)]
struct AudioWriterContext {
    asset_writer: Retained<AVAssetWriter>,
    writer_input: Retained<AVAssetWriterInput>,
}

unsafe impl Send for AudioWriterContext {}
unsafe impl Sync for AudioWriterContext {}

static WRITER_CONTEXT: Mutex<Option<AudioWriterContext>> = Mutex::new(None);

// Dispatch FFI
#[link(name = "System")]
extern "C" {
    fn dispatch_queue_create(label: *const i8, attr: *const c_void) -> *mut c_void;
}

// CoreFoundation run loop FFI
#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFRunLoopRunInMode(mode: *const c_void, seconds: f64, return_after_source_handled: bool) -> i32;
    static kCFRunLoopDefaultMode: *const c_void;
}

// Delegate method for audio sample capture
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

        if sample_buffer.is_null() {
            return;
        }

        let sample_num = SAMPLE_COUNT.fetch_add(1, Ordering::SeqCst);
        if sample_num == 0 {
            println!("  First audio sample received!");
        }

        // Append the audio sample buffer to the asset writer
        let ctx_guard = WRITER_CONTEXT.lock().unwrap();
        if let Some(ref ctx) = *ctx_guard {
            // Convert raw pointer to objc2 reference
            let sample_buffer_obj: &CMSampleBuffer = &*(sample_buffer as *const CMSampleBuffer);

            if ctx.writer_input.isReadyForMoreMediaData() {
                let success: Bool =
                    msg_send![&ctx.writer_input, appendSampleBuffer: sample_buffer_obj];
                if !success.as_bool() {
                    eprintln!("Failed to append audio sample buffer");
                }
            }
        }
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

        // Create asset writer for M4A (MPEG-4 audio)
        let file_type = ns_string!("com.apple.m4a-audio");

        let asset_writer_result =
            AVAssetWriter::assetWriterWithURL_fileType_error(&output_url, file_type);

        let asset_writer = match asset_writer_result {
            Ok(w) => w,
            Err(e) => return Err(format!("Failed to create asset writer: {:?}", e)),
        };

        // Create audio output settings for AAC encoding
        let format_key = NSString::from_str(AV_FORMAT_ID_KEY);
        let sample_rate_key = NSString::from_str(AV_SAMPLE_RATE_KEY);
        let channels_key = NSString::from_str(AV_NUMBER_OF_CHANNELS_KEY);
        let bitrate_key = NSString::from_str(AV_ENCODER_BIT_RATE_KEY);

        let format_value: Retained<NSNumber> = msg_send![class!(NSNumber), numberWithUnsignedInt: K_AUDIO_FORMAT_MPEG4_AAC];
        let sample_rate_value: Retained<NSNumber> = msg_send![class!(NSNumber), numberWithDouble: SAMPLE_RATE];
        let channels_value: Retained<NSNumber> = msg_send![class!(NSNumber), numberWithUnsignedInt: NUM_CHANNELS];
        let bitrate_value: Retained<NSNumber> = msg_send![class!(NSNumber), numberWithInt: 128000i32]; // 128 kbps

        // Build the settings dictionary
        let keys: [&NSString; 4] = [&format_key, &sample_rate_key, &channels_key, &bitrate_key];
        let objects: [&NSNumber; 4] = [&format_value, &sample_rate_value, &channels_value, &bitrate_value];

        let audio_settings: Retained<NSDictionary<NSString, NSObject>> = msg_send![
            class!(NSDictionary),
            dictionaryWithObjects: objects.as_ptr(),
            forKeys: keys.as_ptr(),
            count: 4usize
        ];

        // Create asset writer input for audio
        let media_type = ns_string!("soun");
        let writer_input: Retained<AVAssetWriterInput> = msg_send![
            class!(AVAssetWriterInput),
            assetWriterInputWithMediaType: media_type,
            outputSettings: &*audio_settings
        ];

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
            timescale: SAMPLE_RATE as i32,
            flags: objc2_core_media::CMTimeFlags(1),
            epoch: 0,
        };
        let _: () = msg_send![&asset_writer, startSessionAtSourceTime: zero_time];

        Ok((asset_writer, writer_input))
    }
}

fn main() {
    println!("Microphone to M4A Recording Example");
    println!("====================================");
    println!("Sample rate: {} Hz", SAMPLE_RATE);
    println!("Channels: {}", NUM_CHANNELS);
    println!("Format: AAC (128 kbps)");
    println!("Duration: {} seconds\n", RECORD_DURATION_SECS);

    // Output file path
    let output_path = std::env::current_dir()
        .unwrap()
        .join("output.m4a")
        .to_string_lossy()
        .to_string();

    println!("Output file: {}\n", output_path);

    unsafe {
        // 1. Set up AVAssetWriter for M4A output
        println!("Setting up asset writer...");
        let (asset_writer, writer_input) = match setup_asset_writer(&output_path) {
            Ok((w, i)) => (w, i),
            Err(e) => {
                eprintln!("Failed to set up asset writer: {}", e);
                return;
            }
        };

        // Store writer context for the callback
        {
            let mut ctx = WRITER_CONTEXT.lock().unwrap();
            *ctx = Some(AudioWriterContext {
                asset_writer: asset_writer.clone(),
                writer_input: writer_input.clone(),
            });
        }

        // 2. Set up AVCaptureSession for audio
        println!("Setting up microphone capture...");

        let capture_session = AVCaptureSession::new();
        capture_session.beginConfiguration();

        // Get default audio device (microphone)
        let media_type = AVMediaTypeAudio.expect("AVMediaTypeAudio not available");
        let audio_device = AVCaptureDevice::defaultDeviceWithMediaType(media_type);
        let audio_device = match audio_device {
            Some(d) => d,
            None => {
                eprintln!("No microphone found!");
                return;
            }
        };

        println!("Using microphone: {:?}", audio_device.localizedName());

        // Create device input
        let device_input_result = AVCaptureDeviceInput::deviceInputWithDevice_error(&audio_device);

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
            eprintln!("Cannot add microphone input to session");
            return;
        }

        // Create audio data output
        let audio_output = AVCaptureAudioDataOutput::new();

        // Build delegate class dynamically
        let protocol_name =
            CStr::from_bytes_with_nul(b"AVCaptureAudioDataOutputSampleBufferDelegate\0").unwrap();
        let delegate_protocol = AnyProtocol::get(protocol_name).expect("Protocol not found");

        use objc2::declare::ClassBuilder;

        let class_name = CStr::from_bytes_with_nul(b"AudioCaptureDelegate\0").unwrap();
        let mut builder = ClassBuilder::new(class_name, NSObject::class()).unwrap();
        builder.add_protocol(delegate_protocol);

        // Register the class
        let delegate_class = builder.register();

        // Add the delegate method
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
        let queue_label = b"com.audio.capture.queue\0";
        let callback_queue = dispatch_queue_create(queue_label.as_ptr() as *const i8, ptr::null());

        // Set delegate using properly typed objc_msgSend
        #[link(name = "objc", kind = "dylib")]
        extern "C" {
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
            &*audio_output as *const _ as *const c_void,
            set_delegate_sel,
            &*delegate as *const _ as *const c_void,
            callback_queue,
        );

        // Verify delegate was set
        #[link(name = "objc", kind = "dylib")]
        extern "C" {
            #[link_name = "objc_msgSend"]
            fn objc_msgSend_ptr(receiver: *mut c_void, sel: Sel) -> *mut c_void;
        }

        let current_delegate = objc_msgSend_ptr(
            &*audio_output as *const _ as *mut c_void,
            sel!(sampleBufferDelegate),
        );
        println!("  Delegate set: {}", !current_delegate.is_null());

        // Add output to session
        if capture_session.canAddOutput(&audio_output) {
            capture_session.addOutput(&audio_output);
        } else {
            eprintln!("Cannot add audio output to session");
            return;
        }

        // Commit configuration
        capture_session.commitConfiguration();

        // 3. Start recording
        println!("\nStarting microphone capture...");
        println!("Recording for {} seconds...\n", RECORD_DURATION_SECS);

        capture_session.startRunning();

        let is_running: Bool = msg_send![&capture_session, isRunning];
        println!("  Capture session running: {}", is_running.as_bool());

        // Keep delegate alive
        let _delegate_ref = delegate.clone();

        // Run the run loop to process callbacks
        let start = std::time::Instant::now();
        while start.elapsed() < Duration::from_secs(RECORD_DURATION_SECS) {
            CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.1, false);

            let elapsed = start.elapsed().as_secs();
            static mut LAST_PRINTED: u64 = 0;
            if elapsed > LAST_PRINTED {
                LAST_PRINTED = elapsed;
                println!(
                    "  {} sec - {} audio samples captured",
                    elapsed,
                    SAMPLE_COUNT.load(Ordering::SeqCst)
                );
            }
        }

        // 4. Stop recording
        println!("\nStopping capture...");
        SHOULD_STOP.store(true, Ordering::SeqCst);

        capture_session.stopRunning();

        // Wait a moment for final samples
        std::thread::sleep(Duration::from_millis(200));

        // Mark input as finished and finish writing
        writer_input.markAsFinished();

        println!("Finalizing audio file...");

        use block2::StackBlock;

        let finished = std::sync::Arc::new(AtomicBool::new(false));
        let finished_clone = finished.clone();

        let block = StackBlock::new(move || {
            finished_clone.store(true, Ordering::SeqCst);
        });

        let _: () = msg_send![&asset_writer, finishWritingWithCompletionHandler: &block];

        // Wait for writing to finish
        let timeout = std::time::Instant::now();
        while !finished.load(Ordering::SeqCst) {
            if timeout.elapsed() > Duration::from_secs(10) {
                eprintln!("Timeout waiting for asset writer to finish");
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }

        // Print summary
        let total_samples = SAMPLE_COUNT.load(Ordering::SeqCst);

        println!("\n====================================");
        println!("Recording complete!");
        println!("  Audio samples captured: {}", total_samples);
        println!("  Output file: {}", output_path);

        // Check file size
        if let Ok(metadata) = std::fs::metadata(&output_path) {
            println!(
                "  File size: {:.2} KB",
                metadata.len() as f64 / 1024.0
            );
        }

        println!("\nDone!");
    }
}
