//! Vendored CoreVideo FFI for helpers module.
//!
//! This avoids depending on core-video-sys which has version compatibility issues.

use core_foundation_sys::base::CFAllocatorRef;
use core_foundation_sys::dictionary::CFDictionaryRef;
use core_foundation_sys::string::CFStringRef;
use libc::c_void;

use crate::cv_types::CVPixelBufferRef;

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

    // Functions
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
}
