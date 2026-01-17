//! Query supported properties for a specific encoder configuration.
//!
//! Run with: cargo run --example query_encoder_properties

extern crate core_foundation;
extern crate video_toolbox_sys;

use core_foundation_sys::string::CFStringRef;
use core_foundation_sys::dictionary::CFDictionaryRef;
use std::ptr;
use video_toolbox_sys::utilities::VTCopySupportedPropertyDictionaryForEncoder;

// H.264 codec type
const K_CM_VIDEO_CODEC_TYPE_H264: u32 = 0x61766331; // 'avc1'
const K_CM_VIDEO_CODEC_TYPE_HEVC: u32 = 0x68766331; // 'hvc1'

fn cfstring_to_string(cf_str: CFStringRef) -> String {
    use core_foundation::string::CFString;
    use core_foundation::base::TCFType;
    unsafe {
        CFString::wrap_under_create_rule(cf_str).to_string()
    }
}

fn query_encoder(width: i32, height: i32, codec_type: u32, codec_name: &str) {
    use core_foundation::dictionary::CFDictionary;
    use core_foundation::base::TCFType;

    println!("\n=== {} Encoder Properties ({}x{}) ===\n", codec_name, width, height);

    unsafe {
        let mut encoder_id: CFStringRef = ptr::null();
        let mut supported_properties: CFDictionaryRef = ptr::null();

        let status = VTCopySupportedPropertyDictionaryForEncoder(
            width,
            height,
            codec_type,
            ptr::null(), // Use default encoder
            &mut encoder_id,
            &mut supported_properties,
        );

        if status == 0 {
            if !encoder_id.is_null() {
                println!("Encoder ID: {}", cfstring_to_string(encoder_id));
            }

            if !supported_properties.is_null() {
                let props: CFDictionary = CFDictionary::wrap_under_create_rule(supported_properties as *const _);
                println!("Number of supported properties: {}", props.len());
                println!("\nSupported properties dictionary:\n{:?}", props);
            }
        } else {
            println!("Error querying encoder: OSStatus {}", status);
            println!("(This codec may not be available on your system)");
        }
    }
}

fn main() {
    // Query H.264 encoder for 1920x1080
    query_encoder(1920, 1080, K_CM_VIDEO_CODEC_TYPE_H264, "H.264");

    // Query HEVC encoder for 3840x2160 (4K)
    query_encoder(3840, 2160, K_CM_VIDEO_CODEC_TYPE_HEVC, "HEVC");
}
