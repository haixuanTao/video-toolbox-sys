//! Capture audio from microphone with echo cancellation using Voice Processing I/O.
//!
//! This example uses Apple's Voice Processing I/O Audio Unit which provides:
//! - Acoustic Echo Cancellation (AEC)
//! - Automatic Gain Control (AGC)
//! - Noise Suppression
//!
//! Run with: cargo run --example mic_echo_cancel
//!
//! The output file will be saved as "output_echo_cancel.wav".

use core_foundation_sys::base::OSStatus;
use libc::c_void;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;

// Recording parameters
const SAMPLE_RATE: f64 = 44100.0;
const NUM_CHANNELS: u32 = 1;
const BITS_PER_SAMPLE: u32 = 16;
const RECORD_DURATION_SECS: u64 = 5;

// Audio Unit constants
const K_AUDIO_UNIT_TYPE_OUTPUT: u32 = 0x61756F75; // 'auou'
const K_AUDIO_UNIT_SUBTYPE_VOICE_PROCESSING_IO: u32 = 0x7670696F; // 'vpio'
const K_AUDIO_UNIT_MANUFACTURER_APPLE: u32 = 0x6170706C; // 'appl'

const K_AUDIO_UNIT_SCOPE_GLOBAL: u32 = 0;
const K_AUDIO_UNIT_SCOPE_INPUT: u32 = 1;
const K_AUDIO_UNIT_SCOPE_OUTPUT: u32 = 2;

const K_AUDIO_OUTPUT_UNIT_PROPERTY_ENABLE_IO: u32 = 2003;
const K_AUDIO_UNIT_PROPERTY_STREAM_FORMAT: u32 = 8;
const K_AUDIO_OUTPUT_UNIT_PROPERTY_SET_INPUT_CALLBACK: u32 = 2005;

const K_AUDIO_FORMAT_LINEAR_PCM: u32 = 0x6C70636D; // 'lpcm'
const K_AUDIO_FORMAT_FLAG_IS_SIGNED_INTEGER: u32 = 1 << 2;
const K_AUDIO_FORMAT_FLAG_IS_PACKED: u32 = 1 << 3;

// Audio Unit types
type AudioUnit = *mut c_void;
type AudioComponent = *mut c_void;

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct AudioComponentDescription {
    component_type: u32,
    component_sub_type: u32,
    component_manufacturer: u32,
    component_flags: u32,
    component_flags_mask: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct AudioStreamBasicDescription {
    sample_rate: f64,
    format_id: u32,
    format_flags: u32,
    bytes_per_packet: u32,
    frames_per_packet: u32,
    bytes_per_frame: u32,
    channels_per_frame: u32,
    bits_per_channel: u32,
    reserved: u32,
}

#[repr(C)]
struct AudioBuffer {
    number_channels: u32,
    data_byte_size: u32,
    data: *mut c_void,
}

#[repr(C)]
struct AudioBufferList {
    number_buffers: u32,
    buffers: [AudioBuffer; 1],
}

#[repr(C)]
struct AudioTimeStamp {
    sample_time: f64,
    host_time: u64,
    rate_scalar: f64,
    word_clock_time: u64,
    smtpe_time: [u8; 24], // SMPTETime structure, we don't need it
    flags: u32,
    reserved: u32,
}

#[repr(C)]
struct AURenderCallbackStruct {
    input_proc: extern "C" fn(
        in_ref_con: *mut c_void,
        io_action_flags: *mut u32,
        in_time_stamp: *const AudioTimeStamp,
        in_bus_number: u32,
        in_number_frames: u32,
        io_data: *mut AudioBufferList,
    ) -> OSStatus,
    input_proc_ref_con: *mut c_void,
}

// AudioToolbox FFI
#[link(name = "AudioToolbox", kind = "framework")]
extern "C" {
    fn AudioComponentFindNext(
        in_component: AudioComponent,
        in_desc: *const AudioComponentDescription,
    ) -> AudioComponent;

    fn AudioComponentInstanceNew(
        in_component: AudioComponent,
        out_instance: *mut AudioUnit,
    ) -> OSStatus;

    fn AudioComponentInstanceDispose(in_instance: AudioUnit) -> OSStatus;

    fn AudioUnitInitialize(in_unit: AudioUnit) -> OSStatus;
    fn AudioUnitUninitialize(in_unit: AudioUnit) -> OSStatus;

    fn AudioUnitSetProperty(
        in_unit: AudioUnit,
        in_id: u32,
        in_scope: u32,
        in_element: u32,
        in_data: *const c_void,
        in_data_size: u32,
    ) -> OSStatus;

    fn AudioOutputUnitStart(ci: AudioUnit) -> OSStatus;
    fn AudioOutputUnitStop(ci: AudioUnit) -> OSStatus;

    fn AudioUnitRender(
        in_unit: AudioUnit,
        io_action_flags: *mut u32,
        in_time_stamp: *const AudioTimeStamp,
        in_output_bus_number: u32,
        in_number_frames: u32,
        io_data: *mut AudioBufferList,
    ) -> OSStatus;
}

// Wrapper for AudioUnit to make it Send+Sync
#[allow(dead_code)]
struct AudioUnitWrapper(AudioUnit);
unsafe impl Send for AudioUnitWrapper {}
unsafe impl Sync for AudioUnitWrapper {}

// Global state
static SHOULD_STOP: AtomicBool = AtomicBool::new(false);
static SAMPLE_COUNT: AtomicUsize = AtomicUsize::new(0);
static AUDIO_UNIT: Mutex<Option<AudioUnitWrapper>> = Mutex::new(None);

// Buffer for recorded audio
static AUDIO_BUFFER: Mutex<Vec<i16>> = Mutex::new(Vec::new());

// Input callback - called when audio data is available
extern "C" fn input_render_callback(
    in_ref_con: *mut c_void,
    io_action_flags: *mut u32,
    in_time_stamp: *const AudioTimeStamp,
    _in_bus_number: u32,
    in_number_frames: u32,
    _io_data: *mut AudioBufferList,
) -> OSStatus {
    if SHOULD_STOP.load(Ordering::SeqCst) {
        return 0;
    }

    unsafe {
        let audio_unit = in_ref_con as AudioUnit;

        // Allocate buffer for the audio data
        let bytes_per_frame = (BITS_PER_SAMPLE / 8 * NUM_CHANNELS) as usize;
        let buffer_size = in_number_frames as usize * bytes_per_frame;
        let mut buffer: Vec<u8> = vec![0u8; buffer_size];

        let mut buffer_list = AudioBufferList {
            number_buffers: 1,
            buffers: [AudioBuffer {
                number_channels: NUM_CHANNELS,
                data_byte_size: buffer_size as u32,
                data: buffer.as_mut_ptr() as *mut c_void,
            }],
        };

        // Render (pull) audio from the input
        let status = AudioUnitRender(
            audio_unit,
            io_action_flags,
            in_time_stamp,
            1, // Input bus
            in_number_frames,
            &mut buffer_list,
        );

        if status == 0 {
            // Convert bytes to i16 samples and store
            let samples: &[i16] = std::slice::from_raw_parts(
                buffer.as_ptr() as *const i16,
                in_number_frames as usize * NUM_CHANNELS as usize,
            );

            let mut audio_buf = AUDIO_BUFFER.lock().unwrap();
            audio_buf.extend_from_slice(samples);

            let count = SAMPLE_COUNT.fetch_add(in_number_frames as usize, Ordering::SeqCst);
            if count == 0 {
                println!("  First audio samples received!");
            }
        }
    }

    0
}

fn write_wav_file(path: &str, samples: &[i16], sample_rate: u32, channels: u16) -> std::io::Result<()> {
    let file = File::create(path)?;
    let mut writer = BufWriter::new(file);

    let bits_per_sample: u16 = 16;
    let byte_rate = sample_rate * channels as u32 * bits_per_sample as u32 / 8;
    let block_align = channels * bits_per_sample / 8;
    let data_size = samples.len() as u32 * 2; // 2 bytes per sample

    // RIFF header
    writer.write_all(b"RIFF")?;
    writer.write_all(&(36 + data_size).to_le_bytes())?;
    writer.write_all(b"WAVE")?;

    // fmt chunk
    writer.write_all(b"fmt ")?;
    writer.write_all(&16u32.to_le_bytes())?; // chunk size
    writer.write_all(&1u16.to_le_bytes())?; // PCM format
    writer.write_all(&channels.to_le_bytes())?;
    writer.write_all(&sample_rate.to_le_bytes())?;
    writer.write_all(&byte_rate.to_le_bytes())?;
    writer.write_all(&block_align.to_le_bytes())?;
    writer.write_all(&bits_per_sample.to_le_bytes())?;

    // data chunk
    writer.write_all(b"data")?;
    writer.write_all(&data_size.to_le_bytes())?;

    for sample in samples {
        writer.write_all(&sample.to_le_bytes())?;
    }

    writer.flush()?;
    Ok(())
}

fn main() {
    println!("Microphone with Echo Cancellation Example");
    println!("==========================================");
    println!("Using Voice Processing I/O Audio Unit");
    println!("Features: AEC, AGC, Noise Suppression");
    println!("Sample rate: {} Hz", SAMPLE_RATE);
    println!("Channels: {}", NUM_CHANNELS);
    println!("Duration: {} seconds\n", RECORD_DURATION_SECS);

    let output_path = std::env::current_dir()
        .unwrap()
        .join("output_echo_cancel.wav")
        .to_string_lossy()
        .to_string();

    println!("Output file: {}\n", output_path);

    unsafe {
        // 1. Find Voice Processing I/O Audio Unit
        println!("Setting up Voice Processing I/O...");

        let desc = AudioComponentDescription {
            component_type: K_AUDIO_UNIT_TYPE_OUTPUT,
            component_sub_type: K_AUDIO_UNIT_SUBTYPE_VOICE_PROCESSING_IO,
            component_manufacturer: K_AUDIO_UNIT_MANUFACTURER_APPLE,
            component_flags: 0,
            component_flags_mask: 0,
        };

        let component = AudioComponentFindNext(ptr::null_mut(), &desc);
        if component.is_null() {
            eprintln!("Failed to find Voice Processing I/O component");
            return;
        }
        println!("  Found Voice Processing I/O component");

        // 2. Create Audio Unit instance
        let mut audio_unit: AudioUnit = ptr::null_mut();
        let status = AudioComponentInstanceNew(component, &mut audio_unit);
        if status != 0 {
            eprintln!("Failed to create Audio Unit instance: {}", status);
            return;
        }
        println!("  Created Audio Unit instance");

        // Store for callback
        {
            let mut au = AUDIO_UNIT.lock().unwrap();
            *au = Some(AudioUnitWrapper(audio_unit));
        }

        // 3. Enable input (microphone)
        let enable_flag: u32 = 1;
        let status = AudioUnitSetProperty(
            audio_unit,
            K_AUDIO_OUTPUT_UNIT_PROPERTY_ENABLE_IO,
            K_AUDIO_UNIT_SCOPE_INPUT,
            1, // Input element
            &enable_flag as *const _ as *const c_void,
            std::mem::size_of::<u32>() as u32,
        );
        if status != 0 {
            eprintln!("Failed to enable input: {}", status);
            AudioComponentInstanceDispose(audio_unit);
            return;
        }
        println!("  Enabled microphone input");

        // 4. Set audio format
        let bytes_per_frame = (BITS_PER_SAMPLE / 8) * NUM_CHANNELS;
        let format = AudioStreamBasicDescription {
            sample_rate: SAMPLE_RATE,
            format_id: K_AUDIO_FORMAT_LINEAR_PCM,
            format_flags: K_AUDIO_FORMAT_FLAG_IS_SIGNED_INTEGER | K_AUDIO_FORMAT_FLAG_IS_PACKED,
            bytes_per_packet: bytes_per_frame,
            frames_per_packet: 1,
            bytes_per_frame,
            channels_per_frame: NUM_CHANNELS,
            bits_per_channel: BITS_PER_SAMPLE,
            reserved: 0,
        };

        // Set format on input scope of output element (for the mic data we'll receive)
        let status = AudioUnitSetProperty(
            audio_unit,
            K_AUDIO_UNIT_PROPERTY_STREAM_FORMAT,
            K_AUDIO_UNIT_SCOPE_OUTPUT,
            1, // Input element
            &format as *const _ as *const c_void,
            std::mem::size_of::<AudioStreamBasicDescription>() as u32,
        );
        if status != 0 {
            eprintln!("Failed to set input format: {}", status);
            AudioComponentInstanceDispose(audio_unit);
            return;
        }
        println!("  Set audio format: {} Hz, {} bit, {} ch", SAMPLE_RATE, BITS_PER_SAMPLE, NUM_CHANNELS);

        // 5. Set input callback (for receiving processed mic audio)
        let callback_struct = AURenderCallbackStruct {
            input_proc: input_render_callback,
            input_proc_ref_con: audio_unit,
        };

        let status = AudioUnitSetProperty(
            audio_unit,
            K_AUDIO_OUTPUT_UNIT_PROPERTY_SET_INPUT_CALLBACK,
            K_AUDIO_UNIT_SCOPE_GLOBAL,
            0,
            &callback_struct as *const _ as *const c_void,
            std::mem::size_of::<AURenderCallbackStruct>() as u32,
        );
        if status != 0 {
            eprintln!("Failed to set input callback: {}", status);
            AudioComponentInstanceDispose(audio_unit);
            return;
        }
        println!("  Set input callback");

        // 6. Initialize Audio Unit
        let status = AudioUnitInitialize(audio_unit);
        if status != 0 {
            eprintln!("Failed to initialize Audio Unit: {}", status);
            AudioComponentInstanceDispose(audio_unit);
            return;
        }
        println!("  Initialized Audio Unit");

        // 7. Start recording
        println!("\nStarting recording with echo cancellation...");
        println!("Recording for {} seconds...\n", RECORD_DURATION_SECS);

        let status = AudioOutputUnitStart(audio_unit);
        if status != 0 {
            eprintln!("Failed to start Audio Unit: {}", status);
            AudioUnitUninitialize(audio_unit);
            AudioComponentInstanceDispose(audio_unit);
            return;
        }

        // Record for specified duration
        let start = std::time::Instant::now();
        while start.elapsed() < Duration::from_secs(RECORD_DURATION_SECS) {
            std::thread::sleep(Duration::from_millis(100));

            let elapsed = start.elapsed().as_secs();
            static mut LAST_PRINTED: u64 = 0;
            if elapsed > LAST_PRINTED {
                LAST_PRINTED = elapsed;
                println!(
                    "  {} sec - {} samples captured",
                    elapsed,
                    SAMPLE_COUNT.load(Ordering::SeqCst)
                );
            }
        }

        // 8. Stop recording
        println!("\nStopping...");
        SHOULD_STOP.store(true, Ordering::SeqCst);

        AudioOutputUnitStop(audio_unit);
        AudioUnitUninitialize(audio_unit);
        AudioComponentInstanceDispose(audio_unit);

        // 9. Write WAV file
        println!("Writing WAV file...");
        let audio_data = AUDIO_BUFFER.lock().unwrap();

        match write_wav_file(&output_path, &audio_data, SAMPLE_RATE as u32, NUM_CHANNELS as u16) {
            Ok(_) => println!("  WAV file written successfully"),
            Err(e) => {
                eprintln!("Failed to write WAV file: {}", e);
                return;
            }
        }

        // Summary
        let total_samples = SAMPLE_COUNT.load(Ordering::SeqCst);
        let duration_secs = total_samples as f64 / SAMPLE_RATE;

        println!("\n==========================================");
        println!("Recording complete!");
        println!("  Samples captured: {}", total_samples);
        println!("  Duration: {:.2} seconds", duration_secs);
        println!("  Output: {}", output_path);

        if let Ok(metadata) = std::fs::metadata(&output_path) {
            println!("  File size: {:.2} KB", metadata.len() as f64 / 1024.0);
        }

        println!("\nEcho cancellation was active during recording.");
        println!("Done!");
    }
}
