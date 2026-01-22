//! CoreVideo type definitions and FFI.
//!
//! These are opaque pointer types and functions used by VideoToolbox APIs.
//! They are defined here to avoid a hard dependency on core-video-sys.

use core_foundation_sys::base::CFAllocatorRef;
use core_foundation_sys::dictionary::CFDictionaryRef;
use core_foundation_sys::string::CFStringRef;
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

/// CVReturn success code
pub const kCVReturnSuccess: i32 = 0;

#[link(name = "CoreVideo", kind = "framework")]
extern "C" {
    // Property keys
    pub static kCVPixelBufferPixelFormatTypeKey: CFStringRef;
    pub static kCVPixelBufferWidthKey: CFStringRef;
    pub static kCVPixelBufferHeightKey: CFStringRef;
    pub static kCVPixelBufferCGImageCompatibilityKey: CFStringRef;
    pub static kCVPixelBufferCGBitmapContextCompatibilityKey: CFStringRef;
    pub static kCVPixelBufferIOSurfacePropertiesKey: CFStringRef;

    // CVPixelBuffer functions
    pub fn CVPixelBufferCreate(
        allocator: CFAllocatorRef,
        width: usize,
        height: usize,
        pixelFormatType: u32,
        pixelBufferAttributes: CFDictionaryRef,
        pixelBufferOut: *mut CVPixelBufferRef,
    ) -> i32;

    pub fn CVPixelBufferLockBaseAddress(pixelBuffer: CVPixelBufferRef, lockFlags: u64) -> i32;

    pub fn CVPixelBufferUnlockBaseAddress(pixelBuffer: CVPixelBufferRef, unlockFlags: u64) -> i32;

    pub fn CVPixelBufferGetBaseAddress(pixelBuffer: CVPixelBufferRef) -> *mut c_void;

    pub fn CVPixelBufferGetBytesPerRow(pixelBuffer: CVPixelBufferRef) -> usize;

    pub fn CVPixelBufferGetWidth(pixelBuffer: CVPixelBufferRef) -> usize;

    pub fn CVPixelBufferGetHeight(pixelBuffer: CVPixelBufferRef) -> usize;
}
