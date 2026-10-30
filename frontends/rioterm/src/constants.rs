#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub const PADDING_Y: f32 = 2.0;

pub const MULTI_CLICK_THRESHOLD: std::time::Duration =
    std::time::Duration::from_millis(300);

#[cfg(all(
    feature = "audio",
    not(target_os = "macos"),
    not(target_os = "windows")
))]
pub const BELL_DURATION: std::time::Duration = std::time::Duration::from_millis(200);
