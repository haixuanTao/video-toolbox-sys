//! Builder pattern for VTCompressionSession creation.

#![allow(clippy::missing_transmute_annotations)]

use core_foundation::base::TCFType;
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::CFDictionary;
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;
use core_foundation_sys::base::{kCFAllocatorDefault, CFTypeRef, OSStatus};
use core_foundation_sys::dictionary::CFDictionaryRef;
use core_foundation_sys::string::CFStringRef;
use core_video_sys::kCVPixelBufferPixelFormatTypeKey;
use libc::c_void;
use std::ptr;

use crate::codecs;
use crate::compression::{
    kVTCompressionPropertyKey_AverageBitRate, kVTCompressionPropertyKey_ExpectedFrameRate,
    kVTCompressionPropertyKey_MaxKeyFrameInterval, kVTCompressionPropertyKey_ProfileLevel,
    kVTCompressionPropertyKey_RealTime,
    kVTVideoEncoderSpecification_EnableHardwareAcceleratedVideoEncoder,
    kVTVideoEncoderSpecification_EnableLowLatencyRateControl,
    VTCompressionSessionCreate, VTCompressionSessionInvalidate,
    VTCompressionSessionPrepareToEncodeFrames, VTCompressionSessionRef,
};
use crate::session::VTSessionSetProperty;

/// Configuration for a compression session.
#[derive(Clone)]
pub struct CompressionSessionConfig {
    /// Frame width in pixels
    pub width: i32,
    /// Frame height in pixels
    pub height: i32,
    /// Video codec type (FourCC)
    pub codec: u32,
    /// Source pixel format (FourCC)
    pub pixel_format: u32,
    /// Enable hardware acceleration
    pub hardware_accelerated: bool,
    /// Enable low latency mode
    pub low_latency: bool,
    /// Enable real-time encoding
    pub real_time: bool,
    /// Average bitrate in bits per second
    pub bitrate: Option<i64>,
    /// Expected frame rate
    pub frame_rate: Option<f64>,
    /// Maximum keyframe interval in frames
    pub keyframe_interval: Option<i32>,
    /// H.264/HEVC profile level (CFString reference)
    pub profile_level: Option<CFStringRef>,
}

impl CompressionSessionConfig {
    /// Create a new configuration with default values.
    pub fn new(width: i32, height: i32, codec: u32) -> Self {
        Self {
            width,
            height,
            codec,
            pixel_format: codecs::pixel::BGRA32,
            hardware_accelerated: true,
            low_latency: false,
            real_time: true,
            bitrate: None,
            frame_rate: None,
            keyframe_interval: None,
            profile_level: None,
        }
    }
}

/// Builder for creating VTCompressionSession instances.
///
/// # Example
///
/// ```no_run
/// use video_toolbox_sys::helpers::CompressionSessionBuilder;
/// use video_toolbox_sys::codecs;
///
/// let session = CompressionSessionBuilder::new(1920, 1080, codecs::video::H264)
///     .hardware_accelerated(true)
///     .bitrate(8_000_000)
///     .frame_rate(30.0)
///     .keyframe_interval(30)
///     .real_time(true)
///     .build(|_, _, status, _, sample_buffer| {
///         if status == 0 && !sample_buffer.is_null() {
///             // Handle encoded frame
///         }
///     })
///     .expect("Failed to create compression session");
/// ```
pub struct CompressionSessionBuilder {
    config: CompressionSessionConfig,
}

impl CompressionSessionBuilder {
    /// Create a new builder with the given dimensions and codec.
    pub fn new(width: i32, height: i32, codec: u32) -> Self {
        Self {
            config: CompressionSessionConfig::new(width, height, codec),
        }
    }

    /// Create a builder from an existing configuration.
    pub fn from_config(config: CompressionSessionConfig) -> Self {
        Self { config }
    }

    /// Set the source pixel format (default: BGRA32).
    pub fn pixel_format(mut self, format: u32) -> Self {
        self.config.pixel_format = format;
        self
    }

    /// Enable or disable hardware acceleration (default: true).
    pub fn hardware_accelerated(mut self, enabled: bool) -> Self {
        self.config.hardware_accelerated = enabled;
        self
    }

    /// Enable or disable low latency mode (default: false).
    pub fn low_latency(mut self, enabled: bool) -> Self {
        self.config.low_latency = enabled;
        self
    }

    /// Enable or disable real-time encoding (default: true).
    pub fn real_time(mut self, enabled: bool) -> Self {
        self.config.real_time = enabled;
        self
    }

    /// Set the average bitrate in bits per second.
    pub fn bitrate(mut self, bps: i64) -> Self {
        self.config.bitrate = Some(bps);
        self
    }

    /// Set the expected frame rate.
    pub fn frame_rate(mut self, fps: f64) -> Self {
        self.config.frame_rate = Some(fps);
        self
    }

    /// Set the maximum keyframe interval in frames.
    pub fn keyframe_interval(mut self, frames: i32) -> Self {
        self.config.keyframe_interval = Some(frames);
        self
    }

    /// Set the profile level (e.g., kVTProfileLevel_H264_High_AutoLevel).
    ///
    /// # Safety
    ///
    /// The provided `CFStringRef` must be a valid profile level constant.
    pub fn profile_level(mut self, level: CFStringRef) -> Self {
        self.config.profile_level = Some(level);
        self
    }

    /// Build the compression session with the given output callback.
    ///
    /// The callback is invoked when encoded frames are ready.
    ///
    /// # Arguments
    ///
    /// * `callback` - Function called for each encoded frame with signature:
    ///   `fn(output_ref: *mut c_void, source_ref: *mut c_void, status: OSStatus,
    ///      info_flags: u32, sample_buffer: *mut c_void)`
    ///
    /// # Safety
    ///
    /// The callback must be a valid function pointer that remains valid for the
    /// lifetime of the compression session.
    pub fn build<F>(self, callback: F) -> Result<VTCompressionSessionRef, OSStatus>
    where
        F: Fn(*mut c_void, *mut c_void, OSStatus, u32, *mut c_void) + 'static,
    {
        // Box the callback and leak it - caller is responsible for cleanup
        let callback_box = Box::new(callback);
        let callback_ptr = Box::into_raw(callback_box);

        // SAFETY: The callback pointer is valid and will remain valid for the
        // lifetime of the compression session. The caller is responsible for
        // ensuring proper cleanup.
        unsafe {
            self.build_with_context(
                Some(trampoline::<F>),
                callback_ptr as *mut c_void,
            )
        }
    }

    /// Build the compression session with a raw callback and context pointer.
    ///
    /// This is the low-level API for when you need full control over the callback.
    ///
    /// # Safety
    ///
    /// The callback and context must be valid for the lifetime of the session.
    pub unsafe fn build_with_context(
        self,
        callback: Option<
            extern "C" fn(*mut c_void, *mut c_void, OSStatus, u32, *mut c_void),
        >,
        context: *mut c_void,
    ) -> Result<VTCompressionSessionRef, OSStatus> {
        self.create_session(callback, context)
    }

    unsafe fn create_session(
        self,
        callback: Option<
            extern "C" fn(*mut c_void, *mut c_void, OSStatus, u32, *mut c_void),
        >,
        context: *mut c_void,
    ) -> Result<VTCompressionSessionRef, OSStatus> {
        let config = &self.config;

        // Build encoder specification
        let mut encoder_spec_pairs = Vec::new();

        let hw_key = CFString::wrap_under_get_rule(
            kVTVideoEncoderSpecification_EnableHardwareAcceleratedVideoEncoder as CFStringRef,
        );
        let hw_value = if config.hardware_accelerated {
            CFBoolean::true_value()
        } else {
            CFBoolean::false_value()
        };
        encoder_spec_pairs.push((hw_key.as_CFType(), hw_value.as_CFType()));

        if config.low_latency {
            let ll_key = CFString::wrap_under_get_rule(
                kVTVideoEncoderSpecification_EnableLowLatencyRateControl as CFStringRef,
            );
            encoder_spec_pairs.push((ll_key.as_CFType(), CFBoolean::true_value().as_CFType()));
        }

        let encoder_spec = CFDictionary::from_CFType_pairs(&encoder_spec_pairs);

        // Build source image buffer attributes
        let format_key = CFString::wrap_under_get_rule(kCVPixelBufferPixelFormatTypeKey);
        let width_key = CFString::from_static_string("Width");
        let height_key = CFString::from_static_string("Height");

        let source_attrs = CFDictionary::from_CFType_pairs(&[
            (
                format_key.as_CFType(),
                CFNumber::from(config.pixel_format as i32).as_CFType(),
            ),
            (
                width_key.as_CFType(),
                CFNumber::from(config.width).as_CFType(),
            ),
            (
                height_key.as_CFType(),
                CFNumber::from(config.height).as_CFType(),
            ),
        ]);

        let mut session: VTCompressionSessionRef = ptr::null_mut();

        let status = VTCompressionSessionCreate(
            kCFAllocatorDefault,
            config.width,
            config.height,
            config.codec,
            encoder_spec.as_concrete_TypeRef() as CFDictionaryRef,
            source_attrs.as_concrete_TypeRef() as CFDictionaryRef,
            kCFAllocatorDefault,
            std::mem::transmute(callback),
            context,
            &mut session,
        );

        if status != 0 {
            return Err(status);
        }

        // Configure session properties
        if let Some(profile) = config.profile_level {
            let key = CFString::wrap_under_get_rule(
                kVTCompressionPropertyKey_ProfileLevel as CFStringRef,
            );
            let value = CFString::wrap_under_get_rule(profile);
            VTSessionSetProperty(
                session,
                key.as_concrete_TypeRef(),
                value.as_concrete_TypeRef() as CFTypeRef,
            );
        }

        if let Some(bitrate) = config.bitrate {
            let key = CFString::wrap_under_get_rule(
                kVTCompressionPropertyKey_AverageBitRate as CFStringRef,
            );
            let value = CFNumber::from(bitrate);
            VTSessionSetProperty(
                session,
                key.as_concrete_TypeRef(),
                value.as_concrete_TypeRef() as CFTypeRef,
            );
        }

        if let Some(fps) = config.frame_rate {
            let key = CFString::wrap_under_get_rule(
                kVTCompressionPropertyKey_ExpectedFrameRate as CFStringRef,
            );
            let value = CFNumber::from(fps);
            VTSessionSetProperty(
                session,
                key.as_concrete_TypeRef(),
                value.as_concrete_TypeRef() as CFTypeRef,
            );
        }

        if let Some(interval) = config.keyframe_interval {
            let key = CFString::wrap_under_get_rule(
                kVTCompressionPropertyKey_MaxKeyFrameInterval as CFStringRef,
            );
            let value = CFNumber::from(interval);
            VTSessionSetProperty(
                session,
                key.as_concrete_TypeRef(),
                value.as_concrete_TypeRef() as CFTypeRef,
            );
        }

        if config.real_time {
            let key =
                CFString::wrap_under_get_rule(kVTCompressionPropertyKey_RealTime as CFStringRef);
            VTSessionSetProperty(
                session,
                key.as_concrete_TypeRef(),
                CFBoolean::true_value().as_concrete_TypeRef() as CFTypeRef,
            );
        }

        // Prepare for encoding
        let prep_status = VTCompressionSessionPrepareToEncodeFrames(session);
        if prep_status != 0 {
            VTCompressionSessionInvalidate(session);
            return Err(prep_status);
        }

        Ok(session)
    }
}

/// Trampoline function to invoke the boxed callback.
extern "C" fn trampoline<F>(
    output_ref: *mut c_void,
    source_ref: *mut c_void,
    status: OSStatus,
    info_flags: u32,
    sample_buffer: *mut c_void,
) where
    F: Fn(*mut c_void, *mut c_void, OSStatus, u32, *mut c_void),
{
    unsafe {
        let callback = &*(output_ref as *const F);
        callback(output_ref, source_ref, status, info_flags, sample_buffer);
    }
}
