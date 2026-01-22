//! ObjC delegate creation helpers for AVFoundation capture.

#![allow(dead_code)]

use libc::c_void;
use objc2::declare::ClassBuilder;
use objc2::rc::Retained;
use objc2::runtime::{AnyProtocol, Bool, Sel};
use objc2::{sel, ClassType};
use objc2_foundation::NSObject;
use std::ffi::CStr;
use std::ptr;

/// Callback function signature for capture delegate methods.
///
/// Arguments: (self, _cmd, output, sample_buffer, connection)
pub type DelegateCallback =
    extern "C" fn(*mut c_void, Sel, *mut c_void, *mut c_void, *mut c_void);

// Dispatch queue creation
#[link(name = "System")]
extern "C" {
    fn dispatch_queue_create(label: *const i8, attr: *const c_void) -> *mut c_void;
}

// ObjC runtime for adding methods
#[link(name = "objc", kind = "dylib")]
extern "C" {
    fn class_addMethod(
        cls: *const c_void,
        name: Sel,
        imp: *const c_void,
        types: *const i8,
    ) -> Bool;
}

/// Create an AVCaptureVideoDataOutputSampleBufferDelegate.
///
/// # Example
///
/// ```no_run
/// use video_toolbox_sys::helpers::create_capture_delegate;
/// use objc2::runtime::Sel;
/// use libc::c_void;
///
/// extern "C" fn my_callback(
///     _this: *mut c_void,
///     _cmd: Sel,
///     _output: *mut c_void,
///     sample_buffer: *mut c_void,
///     _connection: *mut c_void,
/// ) {
///     // Handle sample buffer...
/// }
///
/// let delegate = create_capture_delegate(
///     "MyVideoDelegate",
///     "AVCaptureVideoDataOutputSampleBufferDelegate",
///     my_callback,
/// ).expect("Failed to create delegate");
/// ```
pub fn create_capture_delegate(
    class_name: &str,
    protocol_name: &str,
    callback: DelegateCallback,
) -> Result<Retained<NSObject>, &'static str> {
    // Create null-terminated strings
    let class_name_cstr = format!("{}\0", class_name);
    let protocol_name_cstr = format!("{}\0", protocol_name);

    let class_name = CStr::from_bytes_with_nul(class_name_cstr.as_bytes())
        .map_err(|_| "Invalid class name")?;
    let protocol_name = CStr::from_bytes_with_nul(protocol_name_cstr.as_bytes())
        .map_err(|_| "Invalid protocol name")?;

    create_capture_delegate_cstr(class_name, protocol_name, callback)
}

/// Create a capture delegate using CStr names (avoids allocation).
///
/// # Safety
///
/// The provided CStr values must be valid null-terminated strings.
pub fn create_capture_delegate_cstr(
    class_name: &CStr,
    protocol_name: &CStr,
    callback: DelegateCallback,
) -> Result<Retained<NSObject>, &'static str> {
    let protocol = AnyProtocol::get(protocol_name).ok_or("Protocol not found")?;

    let mut builder =
        ClassBuilder::new(class_name, NSObject::class()).ok_or("Failed to create class builder")?;
    builder.add_protocol(protocol);
    let delegate_class = builder.register();

    unsafe {
        let method_sel = sel!(captureOutput:didOutputSampleBuffer:fromConnection:);
        // Method signature: v@:@@@ (void, self, _cmd, output, sampleBuffer, connection)
        let method_types = b"v@:@@@\0";
        let added = class_addMethod(
            delegate_class as *const _ as *const c_void,
            method_sel,
            callback as *const c_void,
            method_types.as_ptr() as *const i8,
        );

        if !added.as_bool() {
            return Err("Failed to add method to delegate class");
        }

        let delegate: Retained<NSObject> = objc2::msg_send![delegate_class, new];
        Ok(delegate)
    }
}

/// Create a dispatch queue for capture callbacks.
///
/// # Example
///
/// ```no_run
/// use video_toolbox_sys::helpers::delegate::create_dispatch_queue;
///
/// let queue = create_dispatch_queue("com.myapp.video.queue");
/// ```
pub fn create_dispatch_queue(label: &str) -> *mut c_void {
    let label_cstr = format!("{}\0", label);
    unsafe { dispatch_queue_create(label_cstr.as_ptr() as *const i8, ptr::null()) }
}

/// Set the sample buffer delegate on an AVCaptureVideoDataOutput or AVCaptureAudioDataOutput.
///
/// # Safety
///
/// - `output` must be a valid AVCaptureVideoDataOutput or AVCaptureAudioDataOutput
/// - `delegate` must be a valid delegate object implementing the appropriate protocol
/// - `queue` must be a valid dispatch queue or null
pub unsafe fn set_sample_buffer_delegate(
    output: *const c_void,
    delegate: *const c_void,
    queue: *const c_void,
) {
    #[link(name = "objc", kind = "dylib")]
    extern "C" {
        #[link_name = "objc_msgSend"]
        fn objc_msgSend_set_delegate(
            receiver: *const c_void,
            sel: Sel,
            delegate: *const c_void,
            queue: *const c_void,
        );
    }

    let set_delegate_sel = sel!(setSampleBufferDelegate:queue:);
    objc_msgSend_set_delegate(output, set_delegate_sel, delegate, queue);
}

/// Helper struct for managing capture delegate lifecycle.
pub struct CaptureDelegate {
    delegate: Retained<NSObject>,
    queue: *mut c_void,
}

impl CaptureDelegate {
    /// Create a new video capture delegate.
    pub fn new_video(class_name: &str, callback: DelegateCallback) -> Result<Self, &'static str> {
        let delegate = create_capture_delegate(
            class_name,
            "AVCaptureVideoDataOutputSampleBufferDelegate",
            callback,
        )?;
        let queue_label = format!("com.videotoolbox.{}.queue", class_name);
        let queue = create_dispatch_queue(&queue_label);
        Ok(Self { delegate, queue })
    }

    /// Create a new audio capture delegate.
    pub fn new_audio(class_name: &str, callback: DelegateCallback) -> Result<Self, &'static str> {
        let delegate = create_capture_delegate(
            class_name,
            "AVCaptureAudioDataOutputSampleBufferDelegate",
            callback,
        )?;
        let queue_label = format!("com.videotoolbox.{}.queue", class_name);
        let queue = create_dispatch_queue(&queue_label);
        Ok(Self { delegate, queue })
    }

    /// Get the delegate object.
    pub fn delegate(&self) -> &Retained<NSObject> {
        &self.delegate
    }

    /// Get the dispatch queue.
    pub fn queue(&self) -> *mut c_void {
        self.queue
    }

    /// Set this delegate on the given capture output.
    ///
    /// # Safety
    ///
    /// The `output` must be a valid AVCaptureVideoDataOutput or AVCaptureAudioDataOutput.
    pub unsafe fn attach_to(&self, output: *const c_void) {
        set_sample_buffer_delegate(
            output,
            &*self.delegate as *const _ as *const c_void,
            self.queue as *const c_void,
        );
    }
}

// CaptureDelegate is not thread-safe due to the raw queue pointer
// but the delegate itself can be sent between threads
unsafe impl Send for CaptureDelegate {}
