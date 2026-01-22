//! FFI bindings and helpers for Apple VideoToolbox framework.
//!
//! VideoToolbox is a low-level framework that provides direct access to hardware
//! encoders and decoders. It provides services for video compression and decompression,
//! and for conversion between raster image formats stored in CoreVideo pixel buffers.
//!
//! # Features
//!
//! - `helpers` - Enable high-level helper utilities (requires additional dependencies)
//!
//! # Example
//!
//! ```no_run
//! use video_toolbox_sys::codecs;
//! use video_toolbox_sys::compression::*;
//!
//! // Use H.264 codec
//! let codec = codecs::video::H264;
//! let pixel_format = codecs::pixel::BGRA32;
//! ```
//!
//! # With helpers feature
//!
//! ```ignore
//! use video_toolbox_sys::helpers::CompressionSessionBuilder;
//! use video_toolbox_sys::codecs;
//!
//! let session = CompressionSessionBuilder::new(1920, 1080, codecs::video::H264)
//!     .hardware_accelerated(true)
//!     .bitrate(8_000_000)
//!     .frame_rate(30.0)
//!     .build_with_context(None, std::ptr::null_mut())
//!     .expect("Failed to create compression session");
//! ```

#![allow(
    non_snake_case,
    non_camel_case_types,
    non_upper_case_globals,
    improper_ctypes
)]
#![cfg(any(target_os = "macos", target_os = "ios"))]

// Document: https://developer.apple.com/documentation/videotoolbox?language=objc

pub mod base;
pub mod compression;
pub mod cv_types;
pub mod decompression;
pub mod errors;
pub mod frame_silo;
pub mod multi_pass_storage;
pub mod pixel_transfer;
pub mod session;
pub mod utilities;

// New modules
pub mod codecs;

#[cfg(feature = "helpers")]
pub mod helpers;
