//! Stream webcam video as CMAF over xoq (QUIC transport).
//!
//! This example demonstrates capturing video from a webcam, encoding it with
//! H.264 using VideoToolbox hardware acceleration, and streaming CMAF
//! segments over QUIC using either:
//! - **MoQ (relay)**: Uses proper group semantics for pub/sub
//! - **iroh (P2P)**: Direct peer-to-peer with length-prefixed framing
//!
//! # MoQ Structure (relay mode)
//!
//! - **Group 0**: Initialization segment (ftyp + moov)
//! - **Group 1, 2, ...**: Media segments (moof + mdat)
//!
//! # iroh Structure (P2P mode)
//!
//! - Length-prefixed frames: [4-byte length][segment data]
//! - First frame: Init segment
//! - Subsequent frames: Media segments
//!
//! # Usage
//!
//! ```bash
//! # MoQ mode (relay) - default
//! cargo run --example camera_xoq_stream --features xoq
//! cargo run --example camera_xoq_stream --features xoq -- anon/my-camera
//! cargo run --example camera_xoq_stream --features xoq -- --relay https://localhost:4443 anon/camera
//!
//! # iroh mode (P2P server)
//! cargo run --example camera_xoq_stream --features xoq -- --iroh
//! # Then connect with the displayed server ID
//! ```
//!
//! # Note
//!
//! Camera permissions may be required on macOS. Grant access when prompted.

use bytes::Bytes;
use core_foundation_sys::base::OSStatus;
use core_media_sys::{CMSampleBufferRef, CMTime};
use libc::c_void;
use moq_native::moq_lite::{self, Broadcast, Origin, Track};
use objc2::rc::Retained;
use objc2::runtime::{Bool, Sel};
use objc2::{class, msg_send};
use objc2_av_foundation::{
    AVCaptureDevice, AVCaptureDeviceInput, AVCaptureSession, AVCaptureVideoDataOutput,
    AVMediaTypeVideo,
};
use objc2_foundation::{ns_string, NSNumber, NSObject};
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
    CmafConfig, CmafMuxer, CompressionSessionBuilder, DelegateCallback, NalExtractor,
};
use xoq::IrohStream;

// Recording parameters
const WIDTH: i32 = 1280;
const HEIGHT: i32 = 720;
const FRAME_RATE: f64 = 30.0;
const BITRATE: i64 = 8_000_000; // 8 Mbps (higher for all-keyframe encoding)
const RECORD_DURATION_SECS: u64 = 30;
const FRAGMENT_DURATION_MS: u32 = 33; // ~1 frame at 30fps for lowest latency

// Global state
static FRAME_COUNT: AtomicUsize = AtomicUsize::new(0);
static ENCODED_FRAMES: AtomicUsize = AtomicUsize::new(0);
static GROUP_COUNT: AtomicUsize = AtomicUsize::new(0);
static SHOULD_STOP: AtomicBool = AtomicBool::new(false);
static INIT_SENT: AtomicBool = AtomicBool::new(false);

/// Transport mode for streaming
enum TransportWriter {
    /// MoQ with proper group semantics
    /// We need to keep the BroadcastProducer alive or the track gets reset
    Moq {
        track: moq_lite::TrackProducer,
        _broadcast: moq_lite::BroadcastProducer,
    },
    /// iroh P2P with length-prefixed framing
    Iroh(std::sync::Arc<tokio::sync::Mutex<Option<IrohStream>>>),
}

// Thread-safe wrapper for streaming context
struct StreamingContext {
    muxer: CmafMuxer,
    extractor: NalExtractor,
    transport: TransportWriter,
    initialized: bool,
    /// Stored init segment for late joiners (prepended to keyframe segments)
    init_segment: Option<Vec<u8>>,
}

unsafe impl Send for StreamingContext {}
unsafe impl Sync for StreamingContext {}

static STREAMING_CONTEXT: Mutex<Option<StreamingContext>> = Mutex::new(None);

// Global compression session
static mut COMPRESSION_SESSION: VTCompressionSessionRef = ptr::null_mut();

// Tokio runtime for async operations in sync callbacks
static TOKIO_RUNTIME: std::sync::OnceLock<tokio::runtime::Handle> = std::sync::OnceLock::new();

// CoreMedia FFI
#[link(name = "CoreMedia", kind = "framework")]
extern "C" {
    fn CMSampleBufferGetImageBuffer(sbuf: *const c_void) -> CVPixelBufferRef;
}

/// Write data as a MoQ frame (each frame becomes its own group).
fn write_moq_group(track: &mut moq_lite::TrackProducer, data: &[u8]) {
    track.write_frame(Bytes::copy_from_slice(data));
}

/// Write data to iroh stream with length prefix.
fn write_iroh_frame(stream: &std::sync::Arc<tokio::sync::Mutex<Option<IrohStream>>>, data: &[u8]) {
    if let Some(handle) = TOKIO_RUNTIME.get() {
        let stream = stream.clone();
        let data = data.to_vec();
        handle.spawn(async move {
            let mut guard = stream.lock().await;
            if let Some(ref mut s) = *guard {
                // Write length prefix (4 bytes, big-endian)
                let len = data.len() as u32;
                if s.write(&len.to_be_bytes()).await.is_err() {
                    return;
                }
                // Write data
                let _ = s.write(&data).await;
            }
        });
    }
}

/// Write segment data using the appropriate transport.
fn write_segment(transport: &mut TransportWriter, data: &[u8]) {
    match transport {
        TransportWriter::Moq { track, .. } => write_moq_group(track, data),
        TransportWriter::Iroh(stream) => write_iroh_frame(stream, data),
    }
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

    let mut ctx_guard = STREAMING_CONTEXT.lock().unwrap();
    let ctx = match ctx_guard.as_mut() {
        Some(c) => c,
        None => return,
    };

    unsafe {
        // Get format description and initialize muxer if needed
        if !ctx.initialized {
            if let Some(format_desc) = ctx.extractor.get_format_description(sample_buffer) {
                match ctx.extractor.extract_parameter_sets(format_desc) {
                    Ok(params) => match ctx.extractor.get_dimensions(format_desc) {
                        Ok(dims) => {
                            // Create initialization segment
                            let init_segment = ctx.muxer.create_init_segment(
                                &params.sps,
                                &params.pps,
                                dims.width,
                                dims.height,
                            );

                            // Store init segment for late joiners
                            ctx.init_segment = Some(init_segment.clone());

                            // Send init segment
                            write_segment(&mut ctx.transport, &init_segment);
                            let group_num = GROUP_COUNT.fetch_add(1, Ordering::SeqCst);
                            INIT_SENT.store(true, Ordering::SeqCst);
                            println!(
                                "  Sent init segment as frame {} ({} bytes)",
                                group_num,
                                init_segment.len()
                            );
                            ctx.initialized = true;
                        }
                        Err(e) => eprintln!("Failed to get dimensions: {}", e),
                    },
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

        // Add frame to muxer - when a segment is complete, send it
        if let Some(segment) = ctx.muxer.add_frame(&nal_units, pts, dts, duration, is_keyframe) {
            // For keyframe segments, prepend init segment for late joiners
            // (they need both init + keyframe to start decoding)
            // Non-keyframe segments are sent as-is since they're smaller
            let data_to_send = if is_keyframe {
                if let Some(ref init) = ctx.init_segment {
                    let mut combined = init.clone();
                    combined.extend_from_slice(&segment);
                    combined
                } else {
                    segment.clone()
                }
            } else {
                segment.clone()
            };

            write_segment(&mut ctx.transport, &data_to_send);
            let group_num = GROUP_COUNT.fetch_add(1, Ordering::SeqCst);
            if is_keyframe {
                println!(
                    "  Sent keyframe segment as frame {} ({} bytes + init)",
                    group_num,
                    segment.len()
                );
            } else {
                println!(
                    "  Sent media segment as frame {} ({} bytes)",
                    group_num,
                    segment.len()
                );
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
            .keyframe_interval(1) // Every frame is a keyframe for lowest latency
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

fn init_logging() {
    if std::env::var("RUST_LOG").is_err() {
        std::env::set_var("RUST_LOG", "warn");
    }
    tracing_subscriber::fmt::init();
}

fn print_help() {
    println!("Usage: camera_xoq_stream [OPTIONS] [PATH]");
    println!();
    println!("Arguments:");
    println!("  [PATH]           MoQ path (default: anon/camera) - ignored in iroh mode");
    println!();
    println!("Options:");
    println!("  --relay <URL>    Custom relay URL (default: https://cdn.moq.dev)");
    println!("  --iroh           Use iroh P2P mode instead of MoQ relay");
    println!("  -h, --help       Show this help message");
    println!();
    println!("Transport Modes:");
    println!("  MoQ (default):   Relay-based pub/sub with group semantics");
    println!("  iroh (--iroh):   Direct P2P with length-prefixed framing");
    println!();
    println!("Examples:");
    println!("  camera_xoq_stream                           # MoQ to default relay");
    println!("  camera_xoq_stream anon/my-camera            # MoQ with custom path");
    println!("  camera_xoq_stream --relay https://... path  # MoQ to custom relay");
    println!("  camera_xoq_stream --iroh                    # iroh P2P server");
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_logging();

    // Store tokio runtime handle for use in sync callbacks
    let _ = TOKIO_RUNTIME.set(tokio::runtime::Handle::current());

    // Parse command line arguments
    let args: Vec<String> = std::env::args().collect();

    let mut path = "anon/camera";
    let mut relay_url: Option<&str> = None;
    let mut use_iroh = false;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--relay" => {
                if i + 1 < args.len() {
                    relay_url = Some(&args[i + 1]);
                    i += 2;
                } else {
                    eprintln!("Error: --relay requires a URL argument");
                    std::process::exit(1);
                }
            }
            "--iroh" => {
                use_iroh = true;
                i += 1;
            }
            "--help" | "-h" => {
                print_help();
                std::process::exit(0);
            }
            _ => {
                path = &args[i];
                i += 1;
            }
        }
    }

    println!("Camera CMAF Streaming over xoq");
    println!("==============================");
    println!("Resolution: {}x{}", WIDTH, HEIGHT);
    println!("Frame rate: {} fps", FRAME_RATE);
    println!("Bitrate: {} Mbps", BITRATE / 1_000_000);
    println!("Fragment duration: {} ms", FRAGMENT_DURATION_MS);
    println!("Duration: {} seconds", RECORD_DURATION_SECS);
    println!(
        "Transport: {}",
        if use_iroh { "iroh (P2P)" } else { "MoQ (relay)" }
    );
    println!();

    // Set up transport
    let transport = if use_iroh {
        // iroh P2P mode
        println!("Starting iroh P2P server...");
        let server = xoq::IrohServerBuilder::new()
            .identity_path(".camera_xoq_key")
            .bind()
            .await?;

        println!("Server started!");
        println!("Server ID: {}", server.id());
        println!("\nWaiting for client connection...");

        let conn = server
            .accept()
            .await?
            .ok_or_else(|| anyhow::anyhow!("No connection"))?;
        println!("Client connected: {}", conn.remote_id());

        // Server opens the stream (since server pushes video to client)
        println!("Opening stream to client...");
        let stream = conn.open_stream().await?;
        println!("Stream established.\n");

        let stream_arc = std::sync::Arc::new(tokio::sync::Mutex::new(Some(stream)));
        TransportWriter::Iroh(stream_arc)
    } else {
        // MoQ relay mode
        println!("MoQ path: {}", path);
        println!(
            "Relay: {}",
            relay_url.unwrap_or("https://cdn.moq.dev (default)")
        );
        println!();
        println!("MoQ Structure:");
        println!("  - Group 0: Init segment");
        println!("  - Group 1+: Media segments");
        println!();
        println!("Connecting to MoQ relay...");

        let url_str = match relay_url {
            Some(url) => format!("{}/{}", url, path),
            None => format!("https://cdn.moq.dev/{}", path),
        };
        let url = url::Url::parse(&url_str)?;

        let client = moq_native::ClientConfig::default().init()?;
        let origin = Origin::produce();
        let _session = match client.connect(url, origin.consumer, None).await {
            Ok(s) => s,
            Err(e) => {
                eprintln!("Failed to connect to MoQ relay: {}", e);
                eprintln!();
                eprintln!("Try using --iroh for P2P mode, or run a local relay.");
                return Err(e.into());
            }
        };

        println!("Connected! Creating video track...");

        let mut broadcast = Broadcast::produce();
        let track = broadcast.producer.create_track(Track {
            name: "video".to_string(),
            priority: 0,
        });
        origin.producer.publish_broadcast("", broadcast.consumer);

        println!("Video track created.\n");
        TransportWriter::Moq {
            track,
            _broadcast: broadcast.producer,
        }
    };

    unsafe {
        // Initialize streaming context
        {
            let muxer = CmafMuxer::new(CmafConfig {
                fragment_duration_ms: FRAGMENT_DURATION_MS,
                timescale: 90000,
            });

            let mut ctx = STREAMING_CONTEXT.lock().unwrap();
            *ctx = Some(StreamingContext {
                muxer,
                extractor: NalExtractor::new(),
                transport,
                initialized: false,
                init_segment: None,
            });
        }

        // Create VideoToolbox compression session
        println!("Creating H.264 compression session...");
        let compression_session = match create_compression_session() {
            Ok(s) => s,
            Err(e) => {
                eprintln!("Failed to create compression session: OSStatus {}", e);
                return Ok(());
            }
        };
        println!("Compression session created successfully!");

        COMPRESSION_SESSION = compression_session;

        // Set up AVCaptureSession
        println!("Setting up camera capture...");

        let capture_session = AVCaptureSession::new();
        capture_session.beginConfiguration();

        let preset = ns_string!("AVCaptureSessionPreset1280x720");
        let can_set: Bool = msg_send![&capture_session, canSetSessionPreset: preset];
        if can_set.as_bool() {
            let _: () = msg_send![&capture_session, setSessionPreset: preset];
        }

        let media_type = AVMediaTypeVideo.expect("AVMediaTypeVideo not available");
        let video_device = AVCaptureDevice::defaultDeviceWithMediaType(media_type);
        let video_device = match video_device {
            Some(d) => d,
            None => {
                eprintln!("No camera device found!");
                return Ok(());
            }
        };

        println!("Using camera: {:?}", video_device.localizedName());

        let device_input_result = AVCaptureDeviceInput::deviceInputWithDevice_error(&video_device);
        let device_input = match device_input_result {
            Ok(i) => i,
            Err(e) => {
                eprintln!("Failed to create device input: {:?}", e);
                return Ok(());
            }
        };

        if capture_session.canAddInput(&device_input) {
            capture_session.addInput(&device_input);
        } else {
            eprintln!("Cannot add camera input to session");
            return Ok(());
        }

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

        let delegate = create_capture_delegate(
            "XoqCameraDelegate",
            "AVCaptureVideoDataOutputSampleBufferDelegate",
            capture_output_did_output as DelegateCallback,
        )
        .expect("Failed to create delegate");

        let callback_queue = create_dispatch_queue("com.videotoolbox.xoq.queue");

        set_sample_buffer_delegate(
            &*video_output as *const _ as *const c_void,
            &*delegate as *const _ as *const c_void,
            callback_queue,
        );

        if capture_session.canAddOutput(&video_output) {
            capture_session.addOutput(&video_output);
        } else {
            eprintln!("Cannot add video output to session");
            return Ok(());
        }

        capture_session.commitConfiguration();

        println!("\nStarting camera capture and streaming...");
        println!("Streaming for {} seconds...\n", RECORD_DURATION_SECS);

        capture_session.startRunning();

        let _delegate_ref = delegate.clone();

        let mut last_printed: u64 = 0;
        run_for_duration(Duration::from_secs(RECORD_DURATION_SECS), |elapsed| {
            let secs = elapsed.as_secs();
            if secs > last_printed {
                last_printed = secs;
                println!(
                    "  {} sec - {} frames captured, {} segments sent",
                    secs,
                    FRAME_COUNT.load(Ordering::SeqCst),
                    GROUP_COUNT.load(Ordering::SeqCst)
                );
            }
        });

        println!("\nStopping capture...");
        SHOULD_STOP.store(true, Ordering::SeqCst);

        capture_session.stopRunning();

        let complete_time = CMTime {
            value: FRAME_COUNT.load(Ordering::SeqCst) as i64,
            timescale: FRAME_RATE as i32,
            flags: 1,
            epoch: 0,
        };
        VTCompressionSessionCompleteFrames(compression_session, complete_time);

        std::thread::sleep(Duration::from_millis(500));

        // Flush remaining frames
        {
            let mut ctx_guard = STREAMING_CONTEXT.lock().unwrap();
            if let Some(ctx) = ctx_guard.as_mut() {
                if let Some(segment) = ctx.muxer.flush() {
                    write_segment(&mut ctx.transport, &segment);
                    let group_num = GROUP_COUNT.fetch_add(1, Ordering::SeqCst);
                    println!(
                        "  Sent final segment as frame {} ({} bytes)",
                        group_num,
                        segment.len()
                    );
                }
            }
        }

        VTCompressionSessionInvalidate(compression_session);

        let total_frames = FRAME_COUNT.load(Ordering::SeqCst);
        let encoded_frames = ENCODED_FRAMES.load(Ordering::SeqCst);
        let total_segments = GROUP_COUNT.load(Ordering::SeqCst);

        println!("\n==============================");
        println!("Streaming complete!");
        println!("  Captured frames: {}", total_frames);
        println!("  Encoded frames: {}", encoded_frames);
        println!(
            "  Segments sent: {} (1 init + {} media)",
            total_segments,
            total_segments.saturating_sub(1)
        );
        println!("  Init segment sent: {}", INIT_SENT.load(Ordering::SeqCst));

        println!("\nDone!");
    }

    Ok(())
}
