/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! Persistent host display preferences for the inspected game.
use std::num::NonZeroU32;
use std::path::PathBuf;

pub const RESOLUTIONS: [(u32, u32); 6] = [
    (480, 320),
    (960, 640),
    (1280, 720),
    (1920, 1080),
    (2560, 1440),
    (3840, 2160),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settings {
    pub resolution: usize,
    pub fullscreen: bool,
    pub scale: u32,
    pub smooth_scrolling: bool,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            resolution: 0,
            fullscreen: false,
            scale: 1,
            smooth_scrolling: false,
        }
    }
}
impl Settings {
    fn encode(self) -> String {
        format!(
            "version=1\nresolution={}\nfullscreen={}\nscale={}\nsmooth_scrolling={}\n",
            self.resolution,
            u8::from(self.fullscreen),
            self.scale,
            u8::from(self.smooth_scrolling)
        )
    }
    fn decode(text: &str) -> Option<Self> {
        let mut lines = text.lines();
        if lines.next()? != "version=1" {
            return None;
        }
        let resolution = lines
            .next()?
            .strip_prefix("resolution=")?
            .parse::<usize>()
            .ok()?;
        let fullscreen = match lines.next()?.strip_prefix("fullscreen=")? {
            "0" => false,
            "1" => true,
            _ => return None,
        };
        let scale = lines.next()?.strip_prefix("scale=")?.parse::<u32>().ok()?;
        let smooth_scrolling = match lines.next() {
            None | Some("smooth_scrolling=0") => false,
            Some("smooth_scrolling=1") => true,
            _ => return None,
        };
        if lines.next().is_some() || resolution >= RESOLUTIONS.len() || !(1..=4).contains(&scale) {
            return None;
        }
        Some(Self {
            resolution,
            fullscreen,
            scale,
            smooth_scrolling,
        })
    }
    /// Read live state, independently of whether its last disk save succeeded.
    pub fn from_options(options: &crate::options::Options) -> Self {
        let scale = options.scale_hack.get();
        let output = options
            .display_resolution
            .unwrap_or((480 * scale, 320 * scale));
        Self {
            resolution: RESOLUTIONS
                .iter()
                .position(|&size| size == output)
                .unwrap_or(0),
            fullscreen: options.fullscreen,
            scale: scale.clamp(1, 4),
            smooth_scrolling: options.smooth_scrolling,
        }
    }
    pub fn apply(self, options: &mut crate::options::Options) {
        options.display_resolution = Some(RESOLUTIONS[self.resolution]);
        options.fullscreen = self.fullscreen;
        options.smooth_scrolling = self.smooth_scrolling;
        options.scale_hack = NonZeroU32::new(self.scale).unwrap();
    }
}
fn path() -> PathBuf {
    // The test launcher creates a new game-save sandbox on every run, but
    // host display preferences should survive a launcher restart.
    #[cfg(feature = "rebellion-test-tools")]
    if let Some(directory) = std::env::var_os("BEBELLION_DISPLAY_SETTINGS_DIR") {
        return PathBuf::from(directory).join("8beet-display-settings-v1");
    }
    crate::paths::user_data_base_path().join("8beet-display-settings-v1")
}
pub fn load_saved() -> Option<Settings> {
    match std::fs::read_to_string(path()) {
        Ok(text) => {
            let settings = Settings::decode(&text);
            if settings.is_none() {
                log!("Invalid display preferences; keeping launcher defaults");
            }
            settings
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            log!("Could not read display preferences: {}", error);
            None
        }
    }
}
fn save(settings: Settings) -> Result<(), String> {
    let path = path();
    let temp = path.with_extension("tmp");
    let mut file = std::fs::File::create(&temp).map_err(|e| e.to_string())?;
    use std::io::Write;
    file.write_all(settings.encode().as_bytes())
        .and_then(|_| file.sync_all())
        .map_err(|e| e.to_string())?;
    drop(file);
    // std::fs::rename replaces existing files on Windows and macOS.
    std::fs::rename(temp, path).map_err(|e| e.to_string())
}
fn choose(window: &sdl2::video::Window, title: &str, text: &str, labels: &[&str]) -> Option<usize> {
    use sdl2::messagebox::*;
    let buttons: Vec<_> = labels
        .iter()
        .enumerate()
        .map(|(i, text)| ButtonData {
            flags: if i == 0 {
                MessageBoxButtonFlag::ESCAPEKEY_DEFAULT
            } else {
                MessageBoxButtonFlag::NOTHING
            },
            button_id: i as i32,
            text,
        })
        .collect();
    match show_message_box(
        MessageBoxFlag::INFORMATION,
        &buttons,
        title,
        text,
        Some(window),
        None,
    ) {
        Ok(ClickedButton::CustomButton(button)) => Some(button.button_id as usize),
        Ok(_) => None,
        Err(error) => {
            log!("Could not show Display Settings: {}", error);
            None
        }
    }
}
/// One pending transaction: neither video nor audio changes before Apply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Action {
    pub display: Settings,
    pub muted: bool,
}
#[repr(C)]
struct NativeSettings {
    indices: [i32; 5],
}
impl NativeSettings {
    fn new(display: Settings, muted: bool) -> Self {
        Self {
            indices: [
                display.resolution as i32,
                display.fullscreen as i32,
                display.scale as i32 - 1,
                display.smooth_scrolling as i32,
                muted as i32,
            ],
        }
    }
    fn finish(self, accepted: bool) -> Option<Action> {
        let [resolution, fullscreen, scale, smooth, muted] = self.indices;
        if !accepted
            || !(0..RESOLUTIONS.len() as i32).contains(&resolution)
            || !(0..=1).contains(&fullscreen)
            || !(0..=3).contains(&scale)
            || !(0..=1).contains(&smooth)
            || !(0..=1).contains(&muted)
        {
            return None;
        }
        Some(Action {
            display: Settings {
                resolution: resolution as usize,
                fullscreen: fullscreen == 1,
                scale: scale as u32 + 1,
                smooth_scrolling: smooth == 1,
            },
            muted: muted == 1,
        })
    }
}
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub fn show(window: &sdl2::video::Window, current: Settings, muted: bool) -> Option<Action> {
    extern "C" {
        fn touchHLE_show_settings(
            window: *mut sdl2_sys::SDL_Window,
            settings: *mut NativeSettings,
        ) -> i32;
    }
    let mut pending = NativeSettings::new(current, muted);
    // The native modal runs synchronously on SDL's parent/main stack and owns
    // its controls until it returns. No Rust callbacks cross the FFI boundary.
    let result = unsafe { touchHLE_show_settings(window.raw(), &mut pending) };
    if result < 0 {
        log!("Could not create native Settings window");
    }
    pending.finish(result == 1)
}
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn show(window: &sdl2::video::Window, current: Settings, muted: bool) -> Option<Action> {
    show_fallback(window, current, muted)
}
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn show_fallback(window: &sdl2::video::Window, current: Settings, muted: bool) -> Option<Action> {
    let mut pending = current;
    let mut muted = muted;
    loop {
        let (w, h) = RESOLUTIONS[pending.resolution];
        let summary = format!("Pending: {} x {} | {} | render {}x\nSmooth scrolling: {}\n{}\nGame logic and sprite frame changes: original 15 FPS.\nTitle, menus and idle locations: 15 FPS.\nApply changes the current game immediately.\nGame proportions are preserved; wide output has side borders.", w, h, if pending.fullscreen { "Fullscreen" } else { "Windowed" }, pending.scale, if pending.smooth_scrolling { "On" } else { "Off" }, if pending.smooth_scrolling { "Camera, character movement and world markers: target 60 FPS." } else { "Location rendering: original 15 FPS." });
        match choose(window, "Display Settings", &summary, &["Cancel", "Resolution", "Window mode", "Render quality", "Smooth scrolling", "Reset to defaults", "Apply", if muted { "Unmute sound" } else { "Mute sound" }]) {
            Some(1) => {
                if let Some(index @ 1..=6) = choose(window, "Output resolution", "Choose an output size. Fullscreen uses the desktop display with a centered output area when it fits.", &["Back", "480 x 320 (Original)", "960 x 640", "1280 x 720 (HD)", "1920 x 1080 (Full HD)", "2560 x 1440 (QHD)", "3840 x 2160 (4K)"]) { pending.resolution = index - 1; }
            }
            Some(2) => match choose(window, "Window mode", "Choose a display mode.", &["Back", "Windowed", "Fullscreen"]) { Some(1) => pending.fullscreen = false, Some(2) => pending.fullscreen = true, _ => {} },
            Some(3) => {
                if let Some(scale @ 1..=4) = choose(window, "Render quality", "Higher scales increase render resolution and GPU work. Original sprites keep their source detail.", &["Back", "1x (Original)", "2x", "3x", "4x"]) { pending.scale = scale as u32; }
            }
            Some(4) => pending.smooth_scrolling = !pending.smooth_scrolling,
            Some(5) => pending = Settings::default(),
            Some(6) => return Some(Action { display: pending, muted }),
            Some(7) => muted = !muted,
            _ => return None,
        }
    }
}

pub fn apply(env: &mut crate::Environment, settings: Settings) -> bool {
    if !env.options.display_service {
        return false;
    }
    crate::frameworks::uikit::rebellion_render::reset(env);
    let old_scale = env.options.scale_hack.get();
    if let Err(error) =
        crate::frameworks::opengles::resize_renderbuffers(env, old_scale, settings.scale)
    {
        log!("Display settings were not applied: {}", error);
        env.on_parent_stack_in_coroutine(move |window, _| {
            let _ = choose(
                window.sdl_window(),
                "Display Settings",
                &format!("Settings were not applied: {}", error),
                &["OK"],
            );
        });
        return false;
    }
    let result = env.on_parent_stack_in_coroutine(move |window, options| {
        window.apply_display_settings(settings)?;
        settings.apply(options);
        Ok::<_, String>(())
    });
    if let Err(error) = result {
        let _ = crate::frameworks::opengles::resize_renderbuffers(env, settings.scale, old_scale);
        log!("Display settings were not applied: {}", error);
        env.on_parent_stack_in_coroutine(move |window, _| {
            let _ = choose(
                window.sdl_window(),
                "Display Settings",
                &format!("Settings were not applied: {}", error),
                &["OK"],
            );
        });
        return false;
    }
    if old_scale != settings.scale {
        crate::frameworks::core_animation::invalidate_display_target(env);
    }
    log!("Applied display preferences live: {:?}", settings);
    if let Err(error) = save(settings) {
        log!("Display settings applied but not saved: {}", error);
        env.on_parent_stack_in_coroutine(move |window, _| {
            let _ = choose(
                window.sdl_window(),
                "Display Settings",
                &format!("Settings applied, but could not be saved: {}", error),
                &["OK"],
            );
        });
    }
    true
}

/// Fit the 3:2 game image inside an output rectangle without stretching.
pub fn fit((w, h): (u32, u32), (aw, ah): (u32, u32)) -> (u32, u32, u32, u32) {
    let scale = (w as f64 / aw as f64).min(h as f64 / ah as f64);
    let fw = (aw as f64 * scale).round() as u32;
    let fh = (ah as f64 * scale).round() as u32;
    ((w - fw) / 2, (h - fh) / 2, fw, fh)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn modal_apply_validates_choices_and_cancel_discards_video_and_audio() {
        let current = Settings {
            resolution: 1,
            scale: 2,
            ..Settings::default()
        };
        let mut pending = NativeSettings::new(current, false);
        pending.indices = [3, 1, 3, 1, 1];
        assert_eq!(pending.finish(false), None);
        assert_eq!(
            NativeSettings::new(current, false).finish(true),
            Some(Action {
                display: current,
                muted: false
            })
        );
        let accepted = NativeSettings {
            indices: [3, 1, 3, 1, 1],
        }
        .finish(true)
        .unwrap();
        assert_eq!(
            accepted.display,
            Settings {
                resolution: 3,
                fullscreen: true,
                scale: 4,
                smooth_scrolling: true
            }
        );
        assert!(accepted.muted);
        for (row, limit) in [6, 2, 4, 2, 2].into_iter().enumerate() {
            for invalid in [-1, limit] {
                let mut pending = NativeSettings::new(current, false);
                pending.indices[row] = invalid;
                assert_eq!(pending.finish(true), None);
            }
        }
    }
    #[test]
    fn preferences_roundtrip_and_reject_invalid_values() {
        for resolution in 0..RESOLUTIONS.len() {
            for scale in 1..=4 {
                for fullscreen in [false, true] {
                    let settings = Settings {
                        resolution,
                        scale,
                        fullscreen,
                        smooth_scrolling: true,
                    };
                    assert_eq!(Settings::decode(&settings.encode()), Some(settings));
                }
            }
        }
        assert_eq!(
            Settings::decode("version=1\nresolution=1\nfullscreen=0\nscale=2\n")
                .unwrap()
                .smooth_scrolling,
            false
        );
        for bad in [
            "",
            "version=1\nresolution=0\nfullscreen=0\nscale=1\nsmooth_scrolling=2\n",
            "version=2\nresolution=0\nfullscreen=0\nscale=1\n",
            "version=1\nresolution=6\nfullscreen=0\nscale=1\n",
            "version=1\nresolution=0\nfullscreen=2\nscale=1\n",
            "version=1\nresolution=0\nfullscreen=0\nscale=0\n",
            "version=1\nresolution=0\nfullscreen=0\nscale=5\n",
            "version=1\nresolution=0\nfullscreen=0\nscale=1\nextra",
        ] {
            assert_eq!(Settings::decode(bad), None);
        }
    }
    #[test]
    fn menu_uses_live_settings_even_when_saved_preferences_are_older() {
        let saved = Settings::default();
        let active = Settings {
            resolution: 2,
            fullscreen: true,
            scale: 3,
            smooth_scrolling: true,
        };
        let mut options = crate::options::Options::default();
        saved.apply(&mut options);
        active.apply(&mut options); // Successful live apply; disk write may fail.
        assert_eq!(Settings::from_options(&options), active);
        // Editing only the window mode must preserve resolution and quality.
        let mut pending = Settings::from_options(&options);
        pending.fullscreen = false;
        assert_eq!(
            pending,
            Settings {
                fullscreen: false,
                ..active
            }
        );
        assert_ne!(pending.resolution, saved.resolution);
    }

    #[test]
    fn popular_outputs_preserve_game_geometry() {
        assert_eq!(fit((1920, 1080), (480, 320)), (150, 0, 1620, 1080));
        assert_eq!(fit((960, 640), (480, 320)), (0, 0, 960, 640));
        assert_eq!(fit((1280, 720), (480, 320)), (100, 0, 1080, 720));
        assert_eq!(fit((3840, 2160), (480, 320)), (300, 0, 3240, 2160));
    }
}
