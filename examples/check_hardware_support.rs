//! Check hardware encoding/decoding support for various codecs.
//!
//! Run with: cargo run --example check_hardware_support

extern crate video_toolbox_sys;

use video_toolbox_sys::utilities::VTIsHardwareDecodeSupported;

// Common codec FourCC codes
const K_CM_VIDEO_CODEC_TYPE_H264: u32 = 0x61766331; // 'avc1'
const K_CM_VIDEO_CODEC_TYPE_HEVC: u32 = 0x68766331; // 'hvc1'
const K_CM_VIDEO_CODEC_TYPE_MPEG4: u32 = 0x6d703476; // 'mp4v'
const K_CM_VIDEO_CODEC_TYPE_VP9: u32 = 0x76703039; // 'vp09'
const K_CM_VIDEO_CODEC_TYPE_AV1: u32 = 0x61763031; // 'av01'

fn fourcc_to_string(code: u32) -> String {
    let bytes = code.to_be_bytes();
    String::from_utf8_lossy(&bytes).to_string()
}

fn main() {
    println!("Checking hardware decode support on this system:\n");

    let codecs = [
        (K_CM_VIDEO_CODEC_TYPE_H264, "H.264/AVC"),
        (K_CM_VIDEO_CODEC_TYPE_HEVC, "H.265/HEVC"),
        (K_CM_VIDEO_CODEC_TYPE_MPEG4, "MPEG-4"),
        (K_CM_VIDEO_CODEC_TYPE_VP9, "VP9"),
        (K_CM_VIDEO_CODEC_TYPE_AV1, "AV1"),
    ];

    for (codec_type, name) in codecs.iter() {
        let supported = unsafe { VTIsHardwareDecodeSupported(*codec_type) };
        let status = if supported != 0 { "✓ Supported" } else { "✗ Not supported" };
        println!(
            "  {:12} ('{}'): {}",
            name,
            fourcc_to_string(*codec_type),
            status
        );
    }

    println!("\nNote: Hardware support depends on your Mac's chip (Intel/Apple Silicon).");
}
