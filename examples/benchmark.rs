//! Benchmark: Native VideoToolbox vs FFmpeg encoding performance
//!
//! This benchmark compares:
//! 1. Native VideoToolbox (direct API calls)
//! 2. FFmpeg with h264_videotoolbox (hardware, same encoder)
//! 3. FFmpeg with libx264 (software, for reference)
//!
//! Run with: cargo run --example benchmark --release

use core_foundation::base::TCFType;
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::CFDictionary;
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;
use core_foundation_sys::base::{kCFAllocatorDefault, CFRelease, CFTypeRef, OSStatus};
use core_foundation_sys::dictionary::CFDictionaryRef;
use core_foundation_sys::string::CFStringRef;
use core_video_sys::{
    kCVPixelBufferCGBitmapContextCompatibilityKey, kCVPixelBufferCGImageCompatibilityKey,
    kCVPixelBufferHeightKey, kCVPixelBufferPixelFormatTypeKey, kCVPixelBufferWidthKey,
    kCVReturnSuccess, CVPixelBufferCreate, CVPixelBufferGetBaseAddress,
    CVPixelBufferGetBytesPerRow, CVPixelBufferLockBaseAddress, CVPixelBufferRef,
    CVPixelBufferUnlockBaseAddress,
};
use std::io::Write;
use std::process::Command;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Instant;
use video_toolbox_sys::compression::{
    kVTCompressionPropertyKey_AverageBitRate, kVTCompressionPropertyKey_ExpectedFrameRate,
    kVTCompressionPropertyKey_MaxKeyFrameInterval, kVTCompressionPropertyKey_ProfileLevel,
    kVTCompressionPropertyKey_RealTime, kVTProfileLevel_H264_High_AutoLevel,
    kVTVideoEncoderSpecification_EnableHardwareAcceleratedVideoEncoder,
    VTCompressionSessionCompleteFrames, VTCompressionSessionCreate,
    VTCompressionSessionEncodeFrame, VTCompressionSessionInvalidate,
    VTCompressionSessionPrepareToEncodeFrames, VTCompressionSessionRef, VTEncodeInfoFlags,
};
use video_toolbox_sys::session::VTSessionSetProperty;

const K_CM_VIDEO_CODEC_TYPE_H264: u32 = 0x61766331;
const K_CV_PIXEL_FORMAT_TYPE_32BGRA: u32 = 0x42475241;

// Benchmark parameters
const WIDTH: i32 = 1920;
const HEIGHT: i32 = 1080;
const NUM_FRAMES: usize = 900; // 30 seconds at 30fps
const FRAME_RATE: f64 = 30.0;
const BITRATE: i64 = 8_000_000;

static ENCODED_FRAMES: AtomicUsize = AtomicUsize::new(0);
static TOTAL_BYTES: AtomicUsize = AtomicUsize::new(0);
static ENCODING_DONE: AtomicBool = AtomicBool::new(false);

#[link(name = "CoreMedia", kind = "framework")]
extern "C" {
    fn CMSampleBufferGetTotalSampleSize(sbuf: *const std::ffi::c_void) -> usize;
}

extern "C" fn compression_callback(
    _: *mut std::ffi::c_void,
    _: *mut std::ffi::c_void,
    status: OSStatus,
    _: VTEncodeInfoFlags,
    sample_buffer: *mut std::ffi::c_void,
) {
    if status != 0 || sample_buffer.is_null() {
        return;
    }

    let size = unsafe { CMSampleBufferGetTotalSampleSize(sample_buffer) };
    TOTAL_BYTES.fetch_add(size, Ordering::SeqCst);

    let count = ENCODED_FRAMES.fetch_add(1, Ordering::SeqCst) + 1;
    if count >= NUM_FRAMES {
        ENCODING_DONE.store(true, Ordering::SeqCst);
    }
}

fn create_test_frame(frame_num: usize) -> CVPixelBufferRef {
    unsafe {
        let mut pixel_buffer: CVPixelBufferRef = ptr::null_mut();

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

        CVPixelBufferCreate(
            kCFAllocatorDefault,
            WIDTH as usize,
            HEIGHT as usize,
            K_CV_PIXEL_FORMAT_TYPE_32BGRA,
            attrs.as_concrete_TypeRef() as CFDictionaryRef,
            &mut pixel_buffer,
        );

        CVPixelBufferLockBaseAddress(pixel_buffer, 0);
        let base = CVPixelBufferGetBaseAddress(pixel_buffer) as *mut u8;
        let stride = CVPixelBufferGetBytesPerRow(pixel_buffer);

        // Create moving gradient pattern
        let offset = (frame_num * 3) % 256;
        for y in 0..HEIGHT as usize {
            let row = base.add(y * stride);
            for x in 0..WIDTH as usize {
                let p = row.add(x * 4);
                *p.add(0) = (((x + y) / 2 + offset) % 256) as u8; // B
                *p.add(1) = ((y + offset) % 256) as u8; // G
                *p.add(2) = ((x + offset) % 256) as u8; // R
                *p.add(3) = 255; // A
            }
        }

        CVPixelBufferUnlockBaseAddress(pixel_buffer, 0);
        pixel_buffer
    }
}

fn benchmark_native_videotoolbox() -> (f64, usize) {
    // Reset counters
    ENCODED_FRAMES.store(0, Ordering::SeqCst);
    TOTAL_BYTES.store(0, Ordering::SeqCst);
    ENCODING_DONE.store(false, Ordering::SeqCst);

    unsafe {
        let hw_key = CFString::wrap_under_get_rule(
            kVTVideoEncoderSpecification_EnableHardwareAcceleratedVideoEncoder as CFStringRef,
        );
        let encoder_spec = CFDictionary::from_CFType_pairs(&[(
            hw_key.as_CFType(),
            CFBoolean::true_value().as_CFType(),
        )]);

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

        VTCompressionSessionCreate(
            kCFAllocatorDefault,
            WIDTH,
            HEIGHT,
            K_CM_VIDEO_CODEC_TYPE_H264,
            encoder_spec.as_concrete_TypeRef() as CFDictionaryRef,
            source_attrs.as_concrete_TypeRef() as CFDictionaryRef,
            kCFAllocatorDefault,
            std::mem::transmute(compression_callback as *const ()),
            ptr::null_mut(),
            &mut session,
        );

        // Configure
        let props: &[(CFStringRef, CFTypeRef)] = &[
            (
                kVTCompressionPropertyKey_ProfileLevel as CFStringRef,
                CFString::wrap_under_get_rule(kVTProfileLevel_H264_High_AutoLevel as CFStringRef)
                    .as_concrete_TypeRef() as CFTypeRef,
            ),
            (
                kVTCompressionPropertyKey_AverageBitRate as CFStringRef,
                CFNumber::from(BITRATE).as_concrete_TypeRef() as CFTypeRef,
            ),
            (
                kVTCompressionPropertyKey_ExpectedFrameRate as CFStringRef,
                CFNumber::from(FRAME_RATE).as_concrete_TypeRef() as CFTypeRef,
            ),
            (
                kVTCompressionPropertyKey_MaxKeyFrameInterval as CFStringRef,
                CFNumber::from(FRAME_RATE as i32).as_concrete_TypeRef() as CFTypeRef,
            ),
            (
                kVTCompressionPropertyKey_RealTime as CFStringRef,
                CFBoolean::false_value().as_concrete_TypeRef() as CFTypeRef,
            ),
        ];

        for (key, value) in props {
            VTSessionSetProperty(session, *key, *value);
        }

        VTCompressionSessionPrepareToEncodeFrames(session);

        // Encode
        let start = Instant::now();

        for frame_num in 0..NUM_FRAMES {
            let pixel_buffer = create_test_frame(frame_num);

            let pts = core_media_sys::CMTime {
                value: frame_num as i64,
                timescale: FRAME_RATE as i32,
                flags: 1,
                epoch: 0,
            };

            let duration = core_media_sys::CMTime {
                value: 1,
                timescale: FRAME_RATE as i32,
                flags: 1,
                epoch: 0,
            };

            let mut info_flags: VTEncodeInfoFlags = 0;
            VTCompressionSessionEncodeFrame(
                session,
                pixel_buffer,
                pts,
                duration,
                ptr::null(),
                ptr::null_mut(),
                &mut info_flags,
            );

            CFRelease(pixel_buffer as CFTypeRef);
        }

        let complete_time = core_media_sys::CMTime {
            value: NUM_FRAMES as i64,
            timescale: FRAME_RATE as i32,
            flags: 1,
            epoch: 0,
        };
        VTCompressionSessionCompleteFrames(session, complete_time);

        // Wait for completion
        while !ENCODING_DONE.load(Ordering::SeqCst) {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }

        let elapsed = start.elapsed().as_secs_f64();

        VTCompressionSessionInvalidate(session);

        (elapsed, TOTAL_BYTES.load(Ordering::SeqCst))
    }
}

fn create_raw_video_file(path: &str) {
    use std::io::BufWriter;
    let file = std::fs::File::create(path).unwrap();
    let mut writer = BufWriter::with_capacity(WIDTH as usize * HEIGHT as usize * 3, file);

    let mut frame_buffer = vec![0u8; WIDTH as usize * HEIGHT as usize * 3];

    for frame_num in 0..NUM_FRAMES {
        let offset = (frame_num * 3) % 256;
        let mut idx = 0;
        for y in 0..HEIGHT as usize {
            for x in 0..WIDTH as usize {
                frame_buffer[idx] = ((x + offset) % 256) as u8;     // R
                frame_buffer[idx + 1] = ((y + offset) % 256) as u8; // G
                frame_buffer[idx + 2] = (((x + y) / 2 + offset) % 256) as u8; // B
                idx += 3;
            }
        }
        writer.write_all(&frame_buffer).unwrap();
    }
    writer.flush().unwrap();
}

fn benchmark_ffmpeg(encoder: &str, raw_path: &str, output_path: &str) -> Option<f64> {
    // Remove output file if exists
    let _ = std::fs::remove_file(output_path);

    let start = Instant::now();

    let mut args = vec![
        "-y".to_string(),
        "-f".to_string(), "rawvideo".to_string(),
        "-pix_fmt".to_string(), "rgb24".to_string(),
        "-s".to_string(), format!("{}x{}", WIDTH, HEIGHT),
        "-r".to_string(), FRAME_RATE.to_string(),
        "-i".to_string(), raw_path.to_string(),
        "-c:v".to_string(), encoder.to_string(),
        "-b:v".to_string(), format!("{}", BITRATE),
    ];

    // Add encoder-specific options
    if encoder == "libx264" {
        args.extend(["-preset".to_string(), "medium".to_string()]);
    } else if encoder == "h264_videotoolbox" {
        args.extend(["-profile:v".to_string(), "high".to_string()]);
    }

    args.extend([
        "-frames:v".to_string(), NUM_FRAMES.to_string(),
        "-f".to_string(), "null".to_string(),
        "-".to_string(),
    ]);

    let status = Command::new("ffmpeg")
        .args(&args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

    match status {
        Ok(s) if s.success() => Some(start.elapsed().as_secs_f64()),
        _ => None,
    }
}

fn main() {
    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║       VideoToolbox vs FFmpeg Encoding Benchmark              ║");
    println!("╚══════════════════════════════════════════════════════════════╝");
    println!();
    println!("Parameters:");
    println!("  Resolution: {}x{}", WIDTH, HEIGHT);
    println!("  Frames: {} ({:.1}s at {} fps)", NUM_FRAMES, NUM_FRAMES as f64 / FRAME_RATE, FRAME_RATE);
    println!("  Bitrate: {} Mbps", BITRATE / 1_000_000);
    println!("  Codec: H.264 High Profile");
    println!();

    // Create raw video for FFmpeg tests
    let raw_path = "/tmp/benchmark_raw.rgb";
    let output_path = "/tmp/benchmark_output.mp4";

    println!("Generating {} test frames...", NUM_FRAMES);
    let gen_start = Instant::now();
    create_raw_video_file(raw_path);
    println!("  Done in {:.2}s\n", gen_start.elapsed().as_secs_f64());

    println!("Running benchmarks (3 iterations each)...\n");

    // Benchmark Native VideoToolbox
    println!("1. Native VideoToolbox (direct API):");
    let mut native_times = Vec::new();
    for i in 1..=3 {
        let (time, bytes) = benchmark_native_videotoolbox();
        let fps = NUM_FRAMES as f64 / time;
        println!("   Run {}: {:.2}s ({:.1} fps, {:.2} MB output)", i, time, fps, bytes as f64 / 1_000_000.0);
        native_times.push(time);
    }
    let native_avg = native_times.iter().sum::<f64>() / native_times.len() as f64;
    println!("   Average: {:.2}s ({:.1} fps)\n", native_avg, NUM_FRAMES as f64 / native_avg);

    // Benchmark FFmpeg with VideoToolbox
    println!("2. FFmpeg + h264_videotoolbox (hardware):");
    let mut ffmpeg_hw_times = Vec::new();
    for i in 1..=3 {
        if let Some(time) = benchmark_ffmpeg("h264_videotoolbox", raw_path, output_path) {
            let fps = NUM_FRAMES as f64 / time;
            println!("   Run {}: {:.2}s ({:.1} fps)", i, time, fps);
            ffmpeg_hw_times.push(time);
        } else {
            println!("   Run {}: FAILED", i);
        }
    }
    let ffmpeg_hw_avg = if !ffmpeg_hw_times.is_empty() {
        let avg = ffmpeg_hw_times.iter().sum::<f64>() / ffmpeg_hw_times.len() as f64;
        println!("   Average: {:.2}s ({:.1} fps)\n", avg, NUM_FRAMES as f64 / avg);
        avg
    } else {
        println!("   FAILED\n");
        f64::MAX
    };

    // Benchmark FFmpeg with libx264 (software)
    println!("3. FFmpeg + libx264 (software):");
    let mut ffmpeg_sw_times = Vec::new();
    for i in 1..=3 {
        if let Some(time) = benchmark_ffmpeg("libx264", raw_path, output_path) {
            let fps = NUM_FRAMES as f64 / time;
            println!("   Run {}: {:.2}s ({:.1} fps)", i, time, fps);
            ffmpeg_sw_times.push(time);
        } else {
            println!("   Run {}: FAILED", i);
        }
    }
    let ffmpeg_sw_avg = if !ffmpeg_sw_times.is_empty() {
        let avg = ffmpeg_sw_times.iter().sum::<f64>() / ffmpeg_sw_times.len() as f64;
        println!("   Average: {:.2}s ({:.1} fps)\n", avg, NUM_FRAMES as f64 / avg);
        avg
    } else {
        println!("   FAILED\n");
        f64::MAX
    };

    // Summary
    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║                         RESULTS                              ║");
    println!("╠══════════════════════════════════════════════════════════════╣");
    println!("║  Encoder                    │  Time   │   FPS   │  vs Native ║");
    println!("╠─────────────────────────────┼─────────┼─────────┼────────────╣");
    println!("║  Native VideoToolbox        │ {:>5.2}s  │ {:>6.1}  │    1.00x   ║",
             native_avg, NUM_FRAMES as f64 / native_avg);
    if ffmpeg_hw_avg < f64::MAX {
        println!("║  FFmpeg + h264_videotoolbox │ {:>5.2}s  │ {:>6.1}  │    {:.2}x   ║",
                 ffmpeg_hw_avg, NUM_FRAMES as f64 / ffmpeg_hw_avg, ffmpeg_hw_avg / native_avg);
    }
    if ffmpeg_sw_avg < f64::MAX {
        println!("║  FFmpeg + libx264           │ {:>5.2}s  │ {:>6.1}  │    {:.2}x   ║",
                 ffmpeg_sw_avg, NUM_FRAMES as f64 / ffmpeg_sw_avg, ffmpeg_sw_avg / native_avg);
    }
    println!("╚══════════════════════════════════════════════════════════════╝");

    // Cleanup
    let _ = std::fs::remove_file(raw_path);
    let _ = std::fs::remove_file(output_path);

    if native_avg < ffmpeg_hw_avg {
        println!("\n✓ Native VideoToolbox is {:.1}% faster than FFmpeg hardware encoding",
                 (ffmpeg_hw_avg / native_avg - 1.0) * 100.0);
    } else {
        println!("\n✗ FFmpeg hardware encoding is {:.1}% faster than Native VideoToolbox",
                 (native_avg / ffmpeg_hw_avg - 1.0) * 100.0);
    }
}
