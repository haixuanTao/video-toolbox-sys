//! High-level helper utilities for working with VideoToolbox.
//!
//! This module provides ergonomic Rust wrappers around common VideoToolbox patterns.
//! Enable with the `helpers` feature flag.
//!
//! # Features
//!
//! - [`CompressionSessionBuilder`] - Fluent API for creating compression sessions
//! - [`PixelBufferConfig`] / [`create_pixel_buffer`] - Utilities for creating CVPixelBuffers
//! - [`create_capture_delegate`] - Safe ObjC delegate creation for AVFoundation
//! - [`run_for_duration`] / [`run_while`] - CoreFoundation run loop helpers
//!
//! # Example
//!
//! ```no_run
//! use video_toolbox_sys::helpers::CompressionSessionBuilder;
//! use video_toolbox_sys::codecs;
//!
//! let session = CompressionSessionBuilder::new(1920, 1080, codecs::video::H264)
//!     .hardware_accelerated(true)
//!     .bitrate(8_000_000)
//!     .frame_rate(30.0)
//!     .build(|_, _, status, _, sample_buffer| {
//!         if status == 0 && !sample_buffer.is_null() {
//!             // Handle encoded frame
//!         }
//!     })
//!     .expect("Failed to create compression session");
//! ```

mod compression_builder;
mod delegate;
mod pixel_buffer;
mod runloop;

pub use compression_builder::{CompressionSessionBuilder, CompressionSessionConfig};
pub use delegate::{create_capture_delegate, DelegateCallback};
pub use pixel_buffer::{create_pixel_buffer, PixelBufferConfig, PixelBufferGuard};
pub use runloop::{run_for_duration, run_while};
