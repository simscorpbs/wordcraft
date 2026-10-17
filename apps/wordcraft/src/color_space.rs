//! Colour-match the window to the display, as Word for Mac's windows are (macOS).
//!
//! WordCraft draws sRGB colours, but the window it renders into isn't tagged with a colour space,
//! so macOS sends the values to the display unconverted and mid tones look lighter than in
//! colour-managed apps: #404040 text shows as 64 where Word's window shows 49 on the same screen.
//! Tagging the window sRGB asks AppKit to convert it to the display's profile. A safe call in the
//! `objc2-app-kit` bindings (our workspace forbids unsafe).

use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSColorSpace};

/// Tag the app window sRGB. False before the window exists or off the main thread.
pub fn tag_window_srgb() -> bool {
    let Some(mtm) = MainThreadMarker::new() else {
        return false;
    };
    let Some(window) = NSApplication::sharedApplication(mtm).windows().firstObject() else {
        return false;
    };
    window.setColorSpace(Some(&NSColorSpace::sRGBColorSpace()));
    true
}
