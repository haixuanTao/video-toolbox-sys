//! Video player client for CMAF streams over xoq (QUIC transport).
//!
//! Receives H.264 CMAF segments, decodes with VideoToolbox, and displays in a window.
//!
//! # Usage
//!
//! ```bash
//! # MoQ mode (relay)
//! cargo run --example camera_xoq_player --features xoq-player
//! cargo run --example camera_xoq_player --features xoq-player -- anon/camera
//!
//! # iroh mode (P2P)
//! cargo run --example camera_xoq_player --features xoq-player -- --iroh <SERVER_ID>
//! ```

use anyhow::{anyhow, Result};
use core_foundation::base::{CFRelease, CFTypeRef, TCFType};
use core_foundation::dictionary::CFDictionary;
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;
use core_foundation_sys::base::OSStatus;
use core_media_sys::CMTime;
use libc::c_void;
use minifb::{Key, Window, WindowOptions};
use moq_native::moq_lite::{Origin, Track};
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use video_toolbox_sys::cv_types::CVPixelBufferRef;
use video_toolbox_sys::decompression::{
    VTDecompressionOutputCallbackRecord, VTDecompressionSessionCreate,
    VTDecompressionSessionDecodeFrame, VTDecompressionSessionInvalidate,
    VTDecompressionSessionRef,
};
use xoq::{IrohClientBuilder, IrohStream};

// Window parameters
const WINDOW_WIDTH: usize = 1280;
const WINDOW_HEIGHT: usize = 720;

// Statistics
static FRAMES_DECODED: AtomicUsize = AtomicUsize::new(0);
static SEGMENTS_RECEIVED: AtomicUsize = AtomicUsize::new(0);
static SHOULD_STOP: AtomicBool = AtomicBool::new(false);

// Frame buffer for display
static FRAME_BUFFER: Mutex<Option<Vec<u32>>> = Mutex::new(None);

// CoreMedia/CoreVideo FFI
#[link(name = "CoreMedia", kind = "framework")]
extern "C" {
    fn CMVideoFormatDescriptionCreateFromH264ParameterSets(
        allocator: *const c_void,
        parameter_set_count: usize,
        parameter_set_pointers: *const *const u8,
        parameter_set_sizes: *const usize,
        nal_unit_header_length: i32,
        format_description_out: *mut *mut c_void,
    ) -> OSStatus;

    fn CMSampleBufferCreate(
        allocator: *const c_void,
        data_buffer: *const c_void,
        data_ready: bool,
        make_data_ready_callback: *const c_void,
        make_data_ready_refcon: *const c_void,
        format_description: *const c_void,
        num_samples: i64,
        num_sample_timing_entries: i64,
        sample_timing_array: *const CMSampleTimingInfo,
        num_sample_size_entries: i64,
        sample_size_array: *const usize,
        sample_buffer_out: *mut *mut c_void,
    ) -> OSStatus;

    fn CMBlockBufferCreateWithMemoryBlock(
        allocator: *const c_void,
        memory_block: *mut c_void,
        block_length: usize,
        block_allocator: *const c_void,
        custom_block_source: *const c_void,
        offset_to_data: usize,
        data_length: usize,
        flags: u32,
        block_buffer_out: *mut *mut c_void,
    ) -> OSStatus;
}

#[link(name = "CoreVideo", kind = "framework")]
extern "C" {
    fn CVPixelBufferLockBaseAddress(pixel_buffer: CVPixelBufferRef, lock_flags: u64) -> i32;
    fn CVPixelBufferUnlockBaseAddress(pixel_buffer: CVPixelBufferRef, unlock_flags: u64) -> i32;
    fn CVPixelBufferGetBaseAddress(pixel_buffer: CVPixelBufferRef) -> *mut c_void;
    fn CVPixelBufferGetWidth(pixel_buffer: CVPixelBufferRef) -> usize;
    fn CVPixelBufferGetHeight(pixel_buffer: CVPixelBufferRef) -> usize;
    fn CVPixelBufferGetBytesPerRow(pixel_buffer: CVPixelBufferRef) -> usize;
}

#[repr(C)]
#[derive(Debug, Copy, Clone)]
struct CMSampleTimingInfo {
    duration: CMTime,
    presentation_time_stamp: CMTime,
    decode_time_stamp: CMTime,
}

/// Parsed CMAF init segment containing codec configuration
struct InitSegment {
    sps: Vec<u8>,
    pps: Vec<u8>,
    width: u32,
    height: u32,
}

/// Parse the init segment to extract SPS/PPS from avcC box
fn parse_init_segment(data: &[u8]) -> Result<InitSegment> {
    // Find avcC box in the init segment
    let mut pos = 0;
    while pos + 8 <= data.len() {
        let box_size = u32::from_be_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]) as usize;
        let box_type = &data[pos + 4..pos + 8];

        if box_size == 0 || pos + box_size > data.len() {
            break;
        }

        if box_type == b"moov" || box_type == b"trak" || box_type == b"mdia"
           || box_type == b"minf" || box_type == b"stbl" {
            // Pure container box, recurse into it (skip 8-byte header)
            if let Ok(result) = parse_init_segment(&data[pos + 8..pos + box_size]) {
                return Ok(result);
            }
        } else if box_type == b"stsd" {
            // stsd is a "full box" with version(1) + flags(3) + entry_count(4) = 8 extra bytes
            // Skip header (8) + version/flags/entry_count (8) = 16 bytes total
            if pos + 16 <= data.len() {
                if let Ok(result) = parse_init_segment(&data[pos + 16..pos + box_size]) {
                    return Ok(result);
                }
            }
        } else if box_type == b"avc1" {
            // avc1 sample entry - skip to avcC
            // avc1 has 78 bytes before nested boxes
            if pos + 86 < pos + box_size {
                if let Ok(result) = parse_init_segment(&data[pos + 86..pos + box_size]) {
                    return Ok(result);
                }
            }
        } else if box_type == b"avcC" {
            // Found avcC box - parse it
            let avcc = &data[pos + 8..pos + box_size];
            return parse_avcc(avcc);
        }

        pos += box_size;
    }

    Err(anyhow!("avcC box not found in init segment"))
}

/// Parse avcC box to extract SPS and PPS
fn parse_avcc(data: &[u8]) -> Result<InitSegment> {
    if data.len() < 7 {
        return Err(anyhow!("avcC too short"));
    }

    let _config_version = data[0];
    let _profile = data[1];
    let _compatibility = data[2];
    let _level = data[3];
    let _length_size_minus_one = data[4] & 0x03;
    let num_sps = (data[5] & 0x1F) as usize;

    let mut pos = 6;
    let mut sps = Vec::new();
    let mut pps = Vec::new();

    // Parse SPS
    for _ in 0..num_sps {
        if pos + 2 > data.len() {
            return Err(anyhow!("Truncated SPS length"));
        }
        let sps_len = u16::from_be_bytes([data[pos], data[pos + 1]]) as usize;
        pos += 2;
        if pos + sps_len > data.len() {
            return Err(anyhow!("Truncated SPS data"));
        }
        sps = data[pos..pos + sps_len].to_vec();
        pos += sps_len;
    }

    // Parse PPS
    if pos >= data.len() {
        return Err(anyhow!("No PPS count"));
    }
    let num_pps = data[pos] as usize;
    pos += 1;

    for _ in 0..num_pps {
        if pos + 2 > data.len() {
            return Err(anyhow!("Truncated PPS length"));
        }
        let pps_len = u16::from_be_bytes([data[pos], data[pos + 1]]) as usize;
        pos += 2;
        if pos + pps_len > data.len() {
            return Err(anyhow!("Truncated PPS data"));
        }
        pps = data[pos..pos + pps_len].to_vec();
        pos += pps_len;
    }

    // Parse dimensions from SPS (simplified - assumes standard SPS structure)
    let (width, height) = parse_sps_dimensions(&sps).unwrap_or((1280, 720));

    Ok(InitSegment {
        sps,
        pps,
        width,
        height,
    })
}

/// Parse SPS to get video dimensions (simplified)
fn parse_sps_dimensions(sps: &[u8]) -> Option<(u32, u32)> {
    if sps.len() < 5 {
        return None;
    }
    // This is a simplified parser - real SPS parsing requires exponential-golomb decoding
    // For now, return None and use defaults
    None
}

/// Parse media segment to extract NAL units from mdat
fn parse_media_segment(data: &[u8]) -> Result<Vec<Vec<u8>>> {
    let mut nal_units = Vec::new();
    let mut pos = 0;

    // Find mdat box
    while pos + 8 <= data.len() {
        let box_size = u32::from_be_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]) as usize;
        let box_type = &data[pos + 4..pos + 8];

        if box_size == 0 || pos + box_size > data.len() {
            break;
        }

        if box_type == b"mdat" {
            // Parse AVCC-formatted NAL units (4-byte length prefix)
            let mdat_data = &data[pos + 8..pos + box_size];
            let mut mdat_pos = 0;

            while mdat_pos + 4 <= mdat_data.len() {
                let nal_len = u32::from_be_bytes([
                    mdat_data[mdat_pos],
                    mdat_data[mdat_pos + 1],
                    mdat_data[mdat_pos + 2],
                    mdat_data[mdat_pos + 3],
                ]) as usize;
                mdat_pos += 4;

                if mdat_pos + nal_len > mdat_data.len() {
                    break;
                }

                nal_units.push(mdat_data[mdat_pos..mdat_pos + nal_len].to_vec());
                mdat_pos += nal_len;
            }
            break;
        }

        pos += box_size;
    }

    Ok(nal_units)
}

/// Decompression output callback
extern "C" fn decompression_callback(
    _decompression_output_ref_con: *mut c_void,
    _source_frame_ref_con: *mut c_void,
    status: OSStatus,
    _info_flags: u32,
    image_buffer: CVPixelBufferRef,
    _pts: CMTime,
    _duration: CMTime,
) {
    if status != 0 {
        eprintln!("Decompression callback error: {}", status);
        return;
    }

    if image_buffer.is_null() {
        eprintln!("Decompression callback: null image buffer");
        return;
    }

    println!("Frame decoded!");

    unsafe {
        // Lock the pixel buffer
        CVPixelBufferLockBaseAddress(image_buffer, 0);

        let base_address = CVPixelBufferGetBaseAddress(image_buffer);
        let width = CVPixelBufferGetWidth(image_buffer);
        let height = CVPixelBufferGetHeight(image_buffer);
        let bytes_per_row = CVPixelBufferGetBytesPerRow(image_buffer);

        if !base_address.is_null() && width > 0 && height > 0 {
            // Convert BGRA to RGB for minifb (which expects 0RGB format)
            let mut buffer = vec![0u32; WINDOW_WIDTH * WINDOW_HEIGHT];

            let src = std::slice::from_raw_parts(base_address as *const u8, bytes_per_row * height);

            for y in 0..height.min(WINDOW_HEIGHT) {
                for x in 0..width.min(WINDOW_WIDTH) {
                    let src_offset = y * bytes_per_row + x * 4;
                    if src_offset + 3 < src.len() {
                        let b = src[src_offset] as u32;
                        let g = src[src_offset + 1] as u32;
                        let r = src[src_offset + 2] as u32;
                        buffer[y * WINDOW_WIDTH + x] = (r << 16) | (g << 8) | b;
                    }
                }
            }

            // Update global frame buffer
            if let Ok(mut fb) = FRAME_BUFFER.lock() {
                *fb = Some(buffer);
            }
        }

        CVPixelBufferUnlockBaseAddress(image_buffer, 0);
    }

    FRAMES_DECODED.fetch_add(1, Ordering::SeqCst);
}

/// Video decoder using VideoToolbox
struct VideoDecoder {
    session: VTDecompressionSessionRef,
    format_desc: *mut c_void,
}

unsafe impl Send for VideoDecoder {}

impl VideoDecoder {
    fn new(init: &InitSegment) -> Result<Self> {
        unsafe {
            // Create format description from SPS/PPS
            let parameter_sets = [init.sps.as_ptr(), init.pps.as_ptr()];
            let parameter_set_sizes = [init.sps.len(), init.pps.len()];

            let mut format_desc: *mut c_void = ptr::null_mut();
            let status = CMVideoFormatDescriptionCreateFromH264ParameterSets(
                ptr::null(),
                2,
                parameter_sets.as_ptr(),
                parameter_set_sizes.as_ptr(),
                4, // NAL unit header length
                &mut format_desc,
            );

            if status != 0 {
                return Err(anyhow!("Failed to create format description: {}", status));
            }

            // Create decompression session
            let callback = VTDecompressionOutputCallbackRecord {
                decompressionOutputCallback: decompression_callback,
                decompressionOutputRefCon: ptr::null_mut(),
            };

            // Destination pixel buffer attributes - request BGRA format
            let pixel_format_key = CFString::new("PixelFormatType");
            let pixel_format_value = CFNumber::from(0x42475241i32); // 'BGRA'
            let width_key = CFString::new("Width");
            let width_value = CFNumber::from(init.width as i32);
            let height_key = CFString::new("Height");
            let height_value = CFNumber::from(init.height as i32);

            let keys = vec![
                pixel_format_key.as_CFType(),
                width_key.as_CFType(),
                height_key.as_CFType(),
            ];
            let values = vec![
                pixel_format_value.as_CFType(),
                width_value.as_CFType(),
                height_value.as_CFType(),
            ];

            let dest_attrs = CFDictionary::from_CFType_pairs(&keys.iter().zip(values.iter())
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect::<Vec<_>>());

            let mut session: VTDecompressionSessionRef = ptr::null_mut();
            let status = VTDecompressionSessionCreate(
                ptr::null(),
                format_desc as *mut _,
                ptr::null(),
                dest_attrs.as_concrete_TypeRef() as *const _,
                &callback,
                &mut session,
            );

            if status != 0 {
                CFRelease(format_desc as CFTypeRef);
                return Err(anyhow!("Failed to create decompression session: {}", status));
            }

            Ok(Self {
                session,
                format_desc,
            })
        }
    }

    fn decode(&mut self, nal_data: &[u8]) -> Result<()> {
        unsafe {
            // Create AVCC-formatted data (4-byte length prefix)
            // Box it to ensure stable memory address
            let mut avcc_data: Box<Vec<u8>> = Box::new(Vec::with_capacity(4 + nal_data.len()));
            avcc_data.extend_from_slice(&(nal_data.len() as u32).to_be_bytes());
            avcc_data.extend_from_slice(nal_data);

            // Create block buffer with copy flag to ensure data is copied
            let mut block_buffer: *mut c_void = ptr::null_mut();
            let status = CMBlockBufferCreateWithMemoryBlock(
                ptr::null(),                           // allocator
                avcc_data.as_mut_ptr() as *mut c_void, // memory block
                avcc_data.len(),                       // block length
                ptr::null(),                           // block allocator (NULL = don't free)
                ptr::null(),                           // custom block source
                0,                                     // offset
                avcc_data.len(),                       // data length
                0,                                     // flags
                &mut block_buffer,
            );

            if status != 0 {
                eprintln!("CMBlockBufferCreate failed: {}", status);
                return Err(anyhow!("Failed to create block buffer: {}", status));
            }

            // Create sample buffer
            let timing = CMSampleTimingInfo {
                duration: CMTime {
                    value: 1,
                    timescale: 30,
                    flags: 1,
                    epoch: 0,
                },
                presentation_time_stamp: CMTime {
                    value: SEGMENTS_RECEIVED.load(Ordering::SeqCst) as i64,
                    timescale: 30,
                    flags: 1,
                    epoch: 0,
                },
                decode_time_stamp: CMTime {
                    value: SEGMENTS_RECEIVED.load(Ordering::SeqCst) as i64,
                    timescale: 30,
                    flags: 1,
                    epoch: 0,
                },
            };

            let sample_size = avcc_data.len();
            let mut sample_buffer: *mut c_void = ptr::null_mut();

            let status = CMSampleBufferCreate(
                ptr::null(),
                block_buffer,
                true,
                ptr::null(),
                ptr::null(),
                self.format_desc,
                1,
                1,
                &timing,
                1,
                &sample_size,
                &mut sample_buffer,
            );

            if status != 0 {
                eprintln!("CMSampleBufferCreate failed: {}", status);
                CFRelease(block_buffer as CFTypeRef);
                return Err(anyhow!("Failed to create sample buffer: {}", status));
            }

            // Decode synchronously (don't use async for debugging)
            let mut info_flags: u32 = 0;
            let status = VTDecompressionSessionDecodeFrame(
                self.session,
                sample_buffer as *mut _,
                0, // Synchronous decode for debugging
                ptr::null_mut(),
                &mut info_flags,
            );

            // Clean up - must happen after synchronous decode completes
            // Note: CMSampleBufferCreate with dataReady=true takes ownership of block_buffer,
            // so releasing sample_buffer also releases block_buffer. Don't release it separately.
            CFRelease(sample_buffer as CFTypeRef);
            // avcc_data (Box) is dropped here, which is safe after sync decode

            if status != 0 {
                eprintln!("VTDecompressionSessionDecodeFrame failed: {}", status);
                return Err(anyhow!("Failed to decode frame: {}", status));
            }

            Ok(())
        }
    }
}

impl Drop for VideoDecoder {
    fn drop(&mut self) {
        unsafe {
            if !self.session.is_null() {
                VTDecompressionSessionInvalidate(self.session);
            }
            if !self.format_desc.is_null() {
                CFRelease(self.format_desc as CFTypeRef);
            }
        }
    }
}

/// Read length-prefixed frame from iroh stream
async fn read_iroh_frame(stream: &mut IrohStream) -> Result<Option<Vec<u8>>> {
    let mut len_buf = [0u8; 4];
    let mut offset = 0;
    while offset < 4 {
        match stream.read(&mut len_buf[offset..]).await? {
            Some(n) if n > 0 => offset += n,
            _ => return Ok(None),
        }
    }

    let len = u32::from_be_bytes(len_buf) as usize;
    if len == 0 {
        return Ok(Some(Vec::new()));
    }

    let mut data = vec![0u8; len];
    let mut offset = 0;
    while offset < len {
        match stream.read(&mut data[offset..]).await? {
            Some(n) if n > 0 => offset += n,
            _ => return Ok(None),
        }
    }

    Ok(Some(data))
}

/// Run iroh receiver with pre-established stream
async fn run_iroh_receiver(
    stream: Arc<tokio::sync::Mutex<IrohStream>>,
    decoder: Arc<Mutex<Option<VideoDecoder>>>,
) -> Result<()> {
    println!("[iroh] Receiving video...\n");

    let mut init_received = false;

    while !SHOULD_STOP.load(Ordering::SeqCst) {
        let data = {
            let mut stream_guard = stream.lock().await;
            read_iroh_frame(&mut *stream_guard).await?
        };

        match data {
            Some(data) if !data.is_empty() => {
                SEGMENTS_RECEIVED.fetch_add(1, Ordering::SeqCst);

                if !init_received {
                    match parse_init_segment(&data) {
                        Ok(init) => {
                            println!("[iroh] Init segment: {}x{}, SPS: {} bytes, PPS: {} bytes",
                                     init.width, init.height, init.sps.len(), init.pps.len());
                            match VideoDecoder::new(&init) {
                                Ok(dec) => {
                                    println!("[iroh] Decoder created successfully!");
                                    *decoder.lock().unwrap() = Some(dec);
                                    init_received = true;
                                }
                                Err(e) => eprintln!("[iroh] Failed to create decoder: {}", e),
                            }
                        }
                        Err(e) => {
                            eprintln!("[iroh] Not an init segment: {}", e);
                        }
                    }
                } else {
                    match parse_media_segment(&data) {
                        Ok(nal_units) => {
                            if let Ok(mut dec_guard) = decoder.lock() {
                                if let Some(ref mut dec) = *dec_guard {
                                    for nal in nal_units {
                                        let _ = dec.decode(&nal);
                                    }
                                }
                            }
                        }
                        Err(e) => eprintln!("[iroh] Failed to parse media segment: {}", e),
                    }
                }
            }
            Some(_) => {}
            None => {
                println!("[iroh] Connection closed.");
                break;
            }
        }
    }

    Ok(())
}

/// Run MoQ client
async fn run_moq_client(relay_url: Option<&str>, path: &str, decoder: Arc<Mutex<Option<VideoDecoder>>>) -> Result<()> {
    let relay = relay_url.unwrap_or("https://cdn.moq.dev");
    println!("Connecting to MoQ relay: {}", relay);
    println!("Path: {}", path);

    let url_str = match relay_url {
        Some(url) => format!("{}/{}", url, path),
        None => format!("https://cdn.moq.dev/{}", path),
    };
    let url = url::Url::parse(&url_str)?;

    let client = moq_native::ClientConfig::default().init()?;
    let mut origin = Origin::produce();
    let _session = client.connect(url, None, origin.producer).await?;

    println!("Connected! Waiting for broadcast...\n");

    let broadcast = loop {
        if SHOULD_STOP.load(Ordering::SeqCst) {
            return Ok(());
        }
        match tokio::time::timeout(std::time::Duration::from_secs(5), origin.consumer.announced()).await {
            Ok(Some((_, Some(b)))) => break b,
            Ok(Some((_, None))) => continue,
            Ok(None) => return Err(anyhow!("No broadcast")),
            Err(_) => {
                println!("  Waiting for broadcast...");
                continue;
            }
        }
    };

    let track_info = Track { name: "video".to_string(), priority: 0 };
    let mut track = broadcast.subscribe_track(&track_info);
    println!("Subscribed to video track.\n");

    let mut init_received = false;

    while !SHOULD_STOP.load(Ordering::SeqCst) {
        match tokio::time::timeout(std::time::Duration::from_secs(5), track.next_group()).await {
            Ok(Ok(Some(mut group))) => {
                while let Ok(Some(data)) = group.read_frame().await {
                    SEGMENTS_RECEIVED.fetch_add(1, Ordering::SeqCst);

                    if !init_received {
                        match parse_init_segment(&data) {
                            Ok(init) => {
                                println!("Init segment: {}x{}, SPS: {} bytes, PPS: {} bytes",
                                         init.width, init.height, init.sps.len(), init.pps.len());
                                match VideoDecoder::new(&init) {
                                    Ok(dec) => {
                                        println!("Decoder created successfully!");
                                        *decoder.lock().unwrap() = Some(dec);
                                        init_received = true;
                                    }
                                    Err(e) => eprintln!("Failed to create decoder: {}", e),
                                }
                            }
                            Err(e) => {
                                eprintln!("Not an init segment: {}", e);
                            }
                        }
                    } else {
                        match parse_media_segment(&data) {
                            Ok(nal_units) => {
                                if nal_units.is_empty() {
                                    eprintln!("No NAL units found in segment");
                                } else {
                                    if let Ok(mut dec_guard) = decoder.lock() {
                                        if let Some(ref mut dec) = *dec_guard {
                                            for (i, nal) in nal_units.iter().enumerate() {
                                                let nal_type = if !nal.is_empty() { nal[0] & 0x1F } else { 0 };
                                                if let Err(e) = dec.decode(nal) {
                                                    eprintln!("Decode NAL {} (type {}, {} bytes) failed: {}",
                                                             i, nal_type, nal.len(), e);
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            Err(e) => eprintln!("Failed to parse media segment: {}", e),
                        }
                    }
                }
            }
            Ok(Ok(None)) => {
                println!("Track ended.");
                break;
            }
            Ok(Err(e)) => {
                eprintln!("Error: {:?}", e);
                break;
            }
            Err(_) => {
                if !init_received {
                    println!("  Waiting for video data...");
                }
            }
        }
    }

    Ok(())
}

fn print_help() {
    println!("Usage: camera_xoq_player [OPTIONS] [PATH_OR_SERVER_ID]");
    println!();
    println!("Options:");
    println!("  --relay <URL>   Custom MoQ relay URL");
    println!("  --iroh          Use iroh P2P mode");
    println!("  -h, --help      Show help");
}

#[tokio::main]
async fn main() -> Result<()> {
    if std::env::var("RUST_LOG").is_err() {
        std::env::set_var("RUST_LOG", "warn");
    }
    tracing_subscriber::fmt::init();

    let args: Vec<String> = std::env::args().collect();
    let mut path_or_id = "anon/camera";
    let mut relay_url: Option<&str> = None;
    let mut use_iroh = false;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--relay" => {
                relay_url = Some(&args[i + 1]);
                i += 2;
            }
            "--iroh" => {
                use_iroh = true;
                i += 1;
            }
            "--help" | "-h" => {
                print_help();
                return Ok(());
            }
            _ => {
                path_or_id = &args[i];
                i += 1;
            }
        }
    }

    if use_iroh && path_or_id == "anon/camera" {
        eprintln!("Error: --iroh requires server ID");
        return Ok(());
    }

    println!("Camera XOQ Video Player");
    println!("=======================");
    println!("Transport: {}", if use_iroh { "iroh (P2P)" } else { "MoQ (relay)" });
    println!();

    // Shared decoder
    let decoder = Arc::new(Mutex::new(None::<VideoDecoder>));
    let decoder_clone = decoder.clone();

    // For iroh mode, establish connection BEFORE creating window
    // Server opens the stream (pushes video), client accepts it
    let iroh_stream: Option<Arc<tokio::sync::Mutex<IrohStream>>> = if use_iroh {
        println!("[iroh] Connecting to server: {}...", path_or_id);
        let conn = IrohClientBuilder::new().connect_str(path_or_id).await?;
        println!("[iroh] Connected to: {}", conn.remote_id());

        println!("[iroh] Waiting for server to open stream...");
        let stream = conn.accept_stream().await?;
        println!("[iroh] Stream received! Server should now start encoding.");

        Some(Arc::new(tokio::sync::Mutex::new(stream)))
    } else {
        None
    };

    // Create window
    let mut window = Window::new(
        "Camera XOQ Player",
        WINDOW_WIDTH,
        WINDOW_HEIGHT,
        WindowOptions {
            resize: false,
            ..WindowOptions::default()
        },
    )?;

    window.set_target_fps(60);

    // Start network client in background
    let path_owned = path_or_id.to_string();
    let relay_owned = relay_url.map(|s| s.to_string());

    tokio::spawn(async move {
        println!("Starting {} client...", if use_iroh { "iroh" } else { "MoQ" });
        let result = if let Some(stream) = iroh_stream {
            // Use pre-established iroh stream
            run_iroh_receiver(stream, decoder_clone).await
        } else {
            // MoQ mode - connect in background
            run_moq_client(relay_owned.as_deref(), &path_owned, decoder_clone).await
        };
        match &result {
            Ok(_) => println!("Client finished successfully."),
            Err(e) => eprintln!("Client error: {:?}", e),
        }
        SHOULD_STOP.store(true, Ordering::SeqCst);
    });

    // Main display loop - keep last frame to avoid black flicker
    let mut display_buffer = vec![0u32; WINDOW_WIDTH * WINDOW_HEIGHT];

    while window.is_open() && !window.is_key_down(Key::Escape) && !SHOULD_STOP.load(Ordering::SeqCst) {
        // Give tokio tasks a chance to run
        tokio::task::yield_now().await;

        // Get latest frame if available, otherwise keep showing the last one
        {
            let mut fb = FRAME_BUFFER.lock().unwrap();
            if let Some(new_frame) = fb.take() {
                display_buffer = new_frame;
            }
        }

        window.update_with_buffer(&display_buffer, WINDOW_WIDTH, WINDOW_HEIGHT)?;

        // Print stats periodically
        let frames = FRAMES_DECODED.load(Ordering::SeqCst);
        let segments = SEGMENTS_RECEIVED.load(Ordering::SeqCst);
        if segments > 0 && segments % 30 == 0 {
            println!("Segments: {}, Frames decoded: {}", segments, frames);
        }
    }

    SHOULD_STOP.store(true, Ordering::SeqCst);
    println!("\nPlayer closed.");
    println!("Total frames decoded: {}", FRAMES_DECODED.load(Ordering::SeqCst));

    Ok(())
}
