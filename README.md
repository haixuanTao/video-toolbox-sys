# video-toolbox-sys

FFI bindings and helpers for Apple VideoToolbox framework.

VideoToolbox is a low-level framework that provides direct access to hardware
encoders and decoders on macOS and iOS. It provides services for video compression
and decompression, and for conversion between raster image formats stored in
CoreVideo pixel buffers.

## Features

- Raw FFI bindings to VideoToolbox APIs
- Codec FourCC constants (H.264, HEVC, AAC, etc.)
- Error code to string conversion
- Optional high-level helpers (with `helpers` feature)

## Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
video-toolbox-sys = { git = "https://github.com/luozijun/rust-videotoolbox-sys" }

# Optional: Enable high-level helpers
video-toolbox-sys = { git = "https://github.com/luozijun/rust-videotoolbox-sys", features = ["helpers"] }
```

**Note:** This crate depends on an unreleased version of `core-video-sys`.
Use the git dependency until version 0.2.0 is published to crates.io.

## Usage

### Basic FFI Usage

```rust
use video_toolbox_sys::codecs;
use video_toolbox_sys::compression::*;
use video_toolbox_sys::errors::vt_error_to_string;

// Use codec constants
let codec = codecs::video::H264;
let pixel_format = codecs::pixel::BGRA32;
```

### With Helpers Feature

```rust
use video_toolbox_sys::helpers::CompressionSessionBuilder;
use video_toolbox_sys::codecs;

let session = CompressionSessionBuilder::new(1920, 1080, codecs::video::H264)
    .hardware_accelerated(true)
    .bitrate(8_000_000)
    .frame_rate(30.0)
    .build(|_, _, status, _, sample_buffer| {
        if status == 0 && !sample_buffer.is_null() {
            // Handle encoded frame
        }
    })
    .expect("Failed to create compression session");
```

## Examples

See the `examples/` directory for complete working examples:

- `encode_dummy_image.rs` - Encode synthetic frames to H.264
- `camera_to_mp4.rs` - Capture from camera and save to MP4
- `av_record.rs` - Record audio + video to MOV

Run an example:

```bash
cargo run --example encode_dummy_image
```

## Requirements

- macOS 10.8+ or iOS 8.0+
- Rust 1.56+

## License

MIT
