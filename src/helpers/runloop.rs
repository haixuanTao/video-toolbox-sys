//! CoreFoundation run loop utilities.

#![allow(dead_code)]

use libc::c_void;
use std::time::{Duration, Instant};

// CoreFoundation run loop FFI
#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFRunLoopGetMain() -> *mut c_void;
    fn CFRunLoopRunInMode(
        mode: *const c_void,
        seconds: f64,
        return_after_source_handled: bool,
    ) -> i32;
    static kCFRunLoopDefaultMode: *const c_void;
}

/// Run loop result codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunLoopResult {
    /// Run loop finished normally
    Finished = 1,
    /// Run loop was stopped
    Stopped = 2,
    /// Run loop timed out
    TimedOut = 3,
    /// A source was handled and returned early
    HandledSource = 4,
}

impl From<i32> for RunLoopResult {
    fn from(value: i32) -> Self {
        match value {
            1 => RunLoopResult::Finished,
            2 => RunLoopResult::Stopped,
            3 => RunLoopResult::TimedOut,
            4 => RunLoopResult::HandledSource,
            _ => RunLoopResult::Finished,
        }
    }
}

/// Run the main run loop for a single iteration with the given timeout.
///
/// # Arguments
///
/// * `timeout` - Maximum time to wait for sources
/// * `return_after_source_handled` - If true, return after handling one source
///
/// # Returns
///
/// The result indicating why the run loop returned.
pub fn run_once(timeout: Duration, return_after_source_handled: bool) -> RunLoopResult {
    unsafe {
        let result = CFRunLoopRunInMode(
            kCFRunLoopDefaultMode,
            timeout.as_secs_f64(),
            return_after_source_handled,
        );
        RunLoopResult::from(result)
    }
}

/// Run the main run loop for the specified duration.
///
/// This function blocks and processes run loop sources for the given duration,
/// which is necessary for AVFoundation callbacks to be delivered.
///
/// # Example
///
/// ```no_run
/// use std::time::Duration;
/// use video_toolbox_sys::helpers::run_for_duration;
///
/// // Run for 5 seconds, processing callbacks
/// run_for_duration(Duration::from_secs(5), |elapsed| {
///     if elapsed.as_secs() % 1 == 0 {
///         println!("Running... {} seconds", elapsed.as_secs());
///     }
/// });
/// ```
pub fn run_for_duration<F>(duration: Duration, mut on_tick: F)
where
    F: FnMut(Duration),
{
    let start = Instant::now();
    while start.elapsed() < duration {
        run_once(Duration::from_millis(100), false);
        on_tick(start.elapsed());
    }
}

/// Run the main run loop while a condition is true.
///
/// # Example
///
/// ```no_run
/// use std::sync::atomic::{AtomicBool, Ordering};
/// use std::time::Duration;
/// use video_toolbox_sys::helpers::run_while;
///
/// let should_run = AtomicBool::new(true);
///
/// run_while(
///     || should_run.load(Ordering::SeqCst),
///     Duration::from_millis(100),
///     Some(Duration::from_secs(60)), // timeout
/// );
/// ```
pub fn run_while<F>(condition: F, interval: Duration, timeout: Option<Duration>) -> bool
where
    F: Fn() -> bool,
{
    let start = Instant::now();

    while condition() {
        if let Some(timeout) = timeout {
            if start.elapsed() >= timeout {
                return false; // Timed out
            }
        }

        run_once(interval, false);
    }

    true // Condition became false
}

/// Run the main run loop until a value is available.
///
/// # Example
///
/// ```no_run
/// use std::sync::{Arc, Mutex};
/// use std::time::Duration;
/// use video_toolbox_sys::helpers::run_until_some;
///
/// let result: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
///
/// // In another thread: *result.lock().unwrap() = Some("done".to_string());
///
/// if let Some(value) = run_until_some(
///     || result.lock().unwrap().clone(),
///     Duration::from_millis(100),
///     Duration::from_secs(10),
/// ) {
///     println!("Got value: {}", value);
/// }
/// ```
pub fn run_until_some<T, F>(
    check: F,
    interval: Duration,
    timeout: Duration,
) -> Option<T>
where
    F: Fn() -> Option<T>,
{
    let start = Instant::now();

    loop {
        if let Some(value) = check() {
            return Some(value);
        }

        if start.elapsed() >= timeout {
            return None;
        }

        run_once(interval, false);
    }
}
