//! Trackpad haptic feedback through `NSHapticFeedbackManager`.

use objc::runtime::{Class, Object};
use objc::{msg_send, sel, sel_impl};

/// `NSHapticFeedbackPatternAlignment`.
const PATTERN_ALIGNMENT: isize = 1;
/// `NSHapticFeedbackPerformanceTimeNow`.
const PERFORMANCE_TIME_NOW: usize = 1;

/// One light alignment tick, now. Does nothing on hardware without a Force Touch trackpad, and
/// macOS drops it when the user turned haptics off.
pub fn perform_alignment_haptic() {
    let Some(manager) = Class::get("NSHapticFeedbackManager") else {
        return;
    };
    // SAFETY: `defaultPerformer` is a class method returning an autoreleased object that conforms
    // to NSHapticFeedbackPerformer; `performFeedbackPattern:performanceTime:` takes an NSInteger
    // and an NSUInteger enum and returns void.
    unsafe {
        let performer: *mut Object = msg_send![manager, defaultPerformer];
        if performer.is_null() {
            return;
        }
        let _: () = msg_send![
            performer,
            performFeedbackPattern: PATTERN_ALIGNMENT
            performanceTime: PERFORMANCE_TIME_NOW
        ];
    }
}

#[cfg(test)]
mod tests {
    /// The class and both selectors exist with these types: a wrong selector raises an
    /// Objective-C exception and aborts the test process.
    #[test]
    fn alignment_haptic_runs_without_a_window() {
        super::perform_alignment_haptic();
    }
}
