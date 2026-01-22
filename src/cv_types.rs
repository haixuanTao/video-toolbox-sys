//! CoreVideo type definitions.
//!
//! These are opaque pointer types used by VideoToolbox APIs.
//! They are defined here to avoid a hard dependency on core-video-sys.

use core_foundation_sys::base::CFTypeRef;
use libc::c_void;

/// Opaque type for CVBuffer
#[repr(C)]
pub struct __CVBuffer {
    _private: c_void,
}

/// Reference to a CoreVideo buffer.
pub type CVBufferRef = *mut __CVBuffer;

/// Reference to a CoreVideo image buffer.
pub type CVImageBufferRef = CVBufferRef;

/// Reference to a CoreVideo pixel buffer.
pub type CVPixelBufferRef = CVImageBufferRef;

/// Reference to a CoreVideo pixel buffer pool.
pub type CVPixelBufferPoolRef = CFTypeRef;
