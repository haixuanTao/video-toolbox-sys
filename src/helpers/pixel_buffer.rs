//! CVPixelBuffer creation and manipulation utilities.

use core_foundation::base::TCFType;
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::CFDictionary;
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;
use core_foundation_sys::base::kCFAllocatorDefault;
use core_foundation_sys::dictionary::CFDictionaryRef;
use std::ptr;

use super::cv_ffi::{
    kCVPixelBufferCGBitmapContextCompatibilityKey, kCVPixelBufferCGImageCompatibilityKey,
    kCVPixelBufferHeightKey, kCVPixelBufferPixelFormatTypeKey, kCVPixelBufferWidthKey,
    kCVReturnSuccess, CVPixelBufferCreate, CVPixelBufferGetBaseAddress,
    CVPixelBufferGetBytesPerRow, CVPixelBufferLockBaseAddress, CVPixelBufferUnlockBaseAddress,
};
use crate::codecs;
use crate::cv_types::CVPixelBufferRef;

/// Configuration for creating a CVPixelBuffer.
#[derive(Clone)]
pub struct PixelBufferConfig {
    /// Width in pixels
    pub width: usize,
    /// Height in pixels
    pub height: usize,
    /// Pixel format (FourCC)
    pub pixel_format: u32,
    /// Enable CoreGraphics compatibility
    pub cg_compatible: bool,
    /// Enable CoreGraphics bitmap context compatibility
    pub cg_bitmap_compatible: bool,
}

impl PixelBufferConfig {
    /// Create a new configuration with default values.
    pub fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            pixel_format: codecs::pixel::BGRA32,
            cg_compatible: true,
            cg_bitmap_compatible: true,
        }
    }

    /// Set the pixel format.
    pub fn pixel_format(mut self, format: u32) -> Self {
        self.pixel_format = format;
        self
    }

    /// Enable or disable CoreGraphics compatibility.
    pub fn cg_compatible(mut self, enabled: bool) -> Self {
        self.cg_compatible = enabled;
        self
    }

    /// Enable or disable CoreGraphics bitmap context compatibility.
    pub fn cg_bitmap_compatible(mut self, enabled: bool) -> Self {
        self.cg_bitmap_compatible = enabled;
        self
    }
}

/// Create a CVPixelBuffer with the given configuration.
///
/// # Example
///
/// ```no_run
/// use video_toolbox_sys::helpers::{create_pixel_buffer, PixelBufferConfig};
///
/// let config = PixelBufferConfig::new(1920, 1080);
/// let pixel_buffer = create_pixel_buffer(&config).expect("Failed to create pixel buffer");
/// ```
///
/// # Safety
///
/// The returned `CVPixelBufferRef` must be released by the caller using `CFRelease`.
pub fn create_pixel_buffer(config: &PixelBufferConfig) -> Result<CVPixelBufferRef, i32> {
    unsafe {
        let mut pixel_buffer: CVPixelBufferRef = ptr::null_mut();

        // Build attributes dictionary
        let format_key = CFString::wrap_under_get_rule(kCVPixelBufferPixelFormatTypeKey);
        let width_key = CFString::wrap_under_get_rule(kCVPixelBufferWidthKey);
        let height_key = CFString::wrap_under_get_rule(kCVPixelBufferHeightKey);

        let mut pairs = vec![
            (
                format_key.as_CFType(),
                CFNumber::from(config.pixel_format as i32).as_CFType(),
            ),
            (
                width_key.as_CFType(),
                CFNumber::from(config.width as i32).as_CFType(),
            ),
            (
                height_key.as_CFType(),
                CFNumber::from(config.height as i32).as_CFType(),
            ),
        ];

        if config.cg_compatible {
            let cg_key = CFString::wrap_under_get_rule(kCVPixelBufferCGImageCompatibilityKey);
            pairs.push((cg_key.as_CFType(), CFBoolean::true_value().as_CFType()));
        }

        if config.cg_bitmap_compatible {
            let cg_bitmap_key =
                CFString::wrap_under_get_rule(kCVPixelBufferCGBitmapContextCompatibilityKey);
            pairs.push((
                cg_bitmap_key.as_CFType(),
                CFBoolean::true_value().as_CFType(),
            ));
        }

        let attrs = CFDictionary::from_CFType_pairs(&pairs);

        let status = CVPixelBufferCreate(
            kCFAllocatorDefault,
            config.width,
            config.height,
            config.pixel_format,
            attrs.as_concrete_TypeRef() as CFDictionaryRef,
            &mut pixel_buffer,
        );

        if status != kCVReturnSuccess {
            return Err(status);
        }

        Ok(pixel_buffer)
    }
}

/// RAII guard for locked CVPixelBuffer access.
///
/// Automatically unlocks the pixel buffer when dropped.
///
/// # Example
///
/// ```no_run
/// use video_toolbox_sys::helpers::{create_pixel_buffer, PixelBufferConfig, PixelBufferGuard};
///
/// let config = PixelBufferConfig::new(1920, 1080);
/// let pixel_buffer = create_pixel_buffer(&config).unwrap();
///
/// {
///     let guard = unsafe { PixelBufferGuard::lock(pixel_buffer).expect("Failed to lock buffer") };
///     let ptr = guard.base_address();
///     let bytes_per_row = guard.bytes_per_row();
///     // Write to buffer...
/// } // Automatically unlocked here
/// ```
pub struct PixelBufferGuard {
    pixel_buffer: CVPixelBufferRef,
    base_address: *mut u8,
    bytes_per_row: usize,
}

impl PixelBufferGuard {
    /// Lock a pixel buffer for CPU access.
    ///
    /// # Safety
    ///
    /// The `pixel_buffer` must be a valid `CVPixelBufferRef`.
    pub unsafe fn lock(pixel_buffer: CVPixelBufferRef) -> Result<Self, i32> {
        let status = CVPixelBufferLockBaseAddress(pixel_buffer, 0);
        if status != kCVReturnSuccess {
            return Err(status);
        }

        let base_address = CVPixelBufferGetBaseAddress(pixel_buffer) as *mut u8;
        let bytes_per_row = CVPixelBufferGetBytesPerRow(pixel_buffer);

        Ok(Self {
            pixel_buffer,
            base_address,
            bytes_per_row,
        })
    }

    /// Get the base address of the locked buffer.
    pub fn base_address(&self) -> *mut u8 {
        self.base_address
    }

    /// Get the bytes per row of the buffer.
    pub fn bytes_per_row(&self) -> usize {
        self.bytes_per_row
    }

    /// Get the pixel buffer reference.
    pub fn pixel_buffer(&self) -> CVPixelBufferRef {
        self.pixel_buffer
    }
}

impl Drop for PixelBufferGuard {
    fn drop(&mut self) {
        unsafe {
            CVPixelBufferUnlockBaseAddress(self.pixel_buffer, 0);
        }
    }
}
