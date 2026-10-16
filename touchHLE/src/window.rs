/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! Abstraction of window setup, OpenGL context creation and event handling.
//!
//! Implemented using the sdl2 crate (a Rust wrapper for SDL2). All usage of
//! SDL should be confined to this module.
//!
//! There is currently no separation of concerns between a single window and
//! window system interaction in general, because it is assumed only one window
//! will be needed for the runtime of the app.

use crate::gles::present::present_frame;
use crate::gles::{create_gles1_ctx_no_parent_stack, GLESContext, GLES};
use crate::image::Image;
use crate::matrix::Matrix;
use crate::options::Options;
use crate::Environment;
use sdl2::keyboard::{Keycode, Scancode};
use sdl2::mouse::MouseButton;
use sdl2::pixels::PixelFormatEnum;
use sdl2::surface::Surface;
use sdl2_sys::SDL_PowerState;
use std::collections::{HashMap, HashSet, VecDeque};
use std::env;
use std::f32::consts::{FRAC_PI_2, PI};
use std::num::NonZeroU32;
use std::ptr::null_mut;
use std::time::{Duration, Instant};

#[allow(non_camel_case_types)]
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum DeviceFamily {
    iPhone,
    iPad,
}
impl std::fmt::Display for DeviceFamily {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, f)
    }
}
impl DeviceFamily {
    pub fn portrait_size(&self) -> (u32, u32) {
        match self {
            DeviceFamily::iPhone => (320, 480),
            DeviceFamily::iPad => (768, 1024),
        }
    }
}
impl TryFrom<u64> for DeviceFamily {
    type Error = ();
    fn try_from(value: u64) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(DeviceFamily::iPhone),
            2 => Ok(DeviceFamily::iPad),
            _ => Err(()),
        }
    }
}
impl TryFrom<&str> for DeviceFamily {
    type Error = ();
    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "iphone" => Ok(DeviceFamily::iPhone),
            "ipad" => Ok(DeviceFamily::iPad),
            _ => Err(()),
        }
    }
}

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum DeviceOrientation {
    Portrait,
    PortraitUpsideDown,
    LandscapeLeft,
    LandscapeRight,
}

fn orientation_rotation(orientation: DeviceOrientation) -> Matrix<2> {
    match orientation {
        DeviceOrientation::Portrait => Matrix::identity(),
        DeviceOrientation::PortraitUpsideDown => Matrix::z_rotation(PI),
        DeviceOrientation::LandscapeLeft => Matrix::z_rotation(-FRAC_PI_2),
        DeviceOrientation::LandscapeRight => Matrix::z_rotation(FRAC_PI_2),
    }
}
fn size_for_orientation(
    family: DeviceFamily,
    orientation: DeviceOrientation,
    scale_hack: NonZeroU32,
) -> (u32, u32) {
    let (width, height) = family.portrait_size();
    let scale_hack = scale_hack.get();
    match orientation {
        DeviceOrientation::Portrait => (width * scale_hack, height * scale_hack),
        DeviceOrientation::PortraitUpsideDown => (width * scale_hack, height * scale_hack),
        DeviceOrientation::LandscapeLeft => (height * scale_hack, width * scale_hack),
        DeviceOrientation::LandscapeRight => (height * scale_hack, width * scale_hack),
    }
}
fn rotate_fullscreen_size(orientation: DeviceOrientation, screen_size: (u32, u32)) -> (u32, u32) {
    let (short_side, long_side) = if screen_size.0 < screen_size.1 {
        (screen_size.0, screen_size.1)
    } else {
        (screen_size.1, screen_size.0)
    };
    match orientation {
        DeviceOrientation::Portrait | DeviceOrientation::PortraitUpsideDown => {
            (short_side, long_side)
        }
        DeviceOrientation::LandscapeLeft | DeviceOrientation::LandscapeRight => {
            (long_side, short_side)
        }
    }
}

fn transform_input_in_viewport(
    (in_x, in_y): (f32, f32),
    (vx, vy, vw, vh): (u32, u32, u32, u32),
    (out_w, out_h): (u32, u32),
    rotation: Matrix<2>,
) -> (f32, f32) {
    let x = (in_x - vx as f32) / vw as f32 - 0.5;
    let y = (in_y - vy as f32) / vh as f32 - 0.5;
    let [x, y] = rotation.inverse().unwrap().transform([x, y]);
    ((x + 0.5) * out_w as f32, (y + 0.5) * out_h as f32)
}

#[cfg(test)]
#[test]
fn landscape_content_input_matches_visible_position() {
    let viewport = (0, 0, 960, 640);
    let output = (480, 320);
    for (screen, expected) in [
        ((0.0, 0.0), (0.0, 0.0)),
        ((480.0, 320.0), (240.0, 160.0)),
        ((960.0, 640.0), (480.0, 320.0)),
    ] {
        assert_eq!(
            transform_input_in_viewport(screen, viewport, output, Matrix::identity()),
            expected
        );
    }
}
/// Tell SDL2 what orientation we want. Only useful on Android.
fn set_sdl2_orientation(orientation: DeviceOrientation) {
    // Despite the name, this hint works on Android too.
    sdl2::hint::set(
        "SDL_IOS_ORIENTATIONS",
        match orientation {
            DeviceOrientation::Portrait => "Portrait",
            // The inversion is deliberate. These probably correspond to
            // iPhone OS content orientations?
            DeviceOrientation::PortraitUpsideDown => "PortraitUpsideDown",
            DeviceOrientation::LandscapeLeft => "LandscapeRight",
            DeviceOrientation::LandscapeRight => "LandscapeLeft",
        },
    );
}

#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum FingerId {
    Mouse,
    Touch(i64),
    VirtualCursor,
    ButtonToTouch(crate::options::Button),
    StickToTouch,
    DpadToTouch,
    KeyboardDirection,
    KeyboardAttack,
}
pub type Coords = (f32, f32);

struct DpadState {
    left: bool,
    right: bool,
    up: bool,
    down: bool,
    active: bool,
}

#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
enum GameKey {
    A,
    D,
    W,
    Left,
    Right,
    Up,
    Space,
    Escape,
}

fn game_key(scancode: Option<Scancode>, keycode: Option<Keycode>) -> Option<GameKey> {
    match scancode {
        Some(Scancode::A) => Some(GameKey::A),
        Some(Scancode::D) => Some(GameKey::D),
        Some(Scancode::W) => Some(GameKey::W),
        Some(Scancode::Left) => Some(GameKey::Left),
        Some(Scancode::Right) => Some(GameKey::Right),
        Some(Scancode::Up) => Some(GameKey::Up),
        Some(Scancode::Space) => Some(GameKey::Space),
        Some(Scancode::Escape) => Some(GameKey::Escape),
        Some(_) => None,
        None => match keycode {
            Some(Keycode::A) => Some(GameKey::A),
            Some(Keycode::D) => Some(GameKey::D),
            Some(Keycode::W) => Some(GameKey::W),
            Some(Keycode::Left) => Some(GameKey::Left),
            Some(Keycode::Right) => Some(GameKey::Right),
            Some(Keycode::Up) => Some(GameKey::Up),
            Some(Keycode::Space) => Some(GameKey::Space),
            Some(Keycode::Escape) => Some(GameKey::Escape),
            _ => None,
        },
    }
}

// SDL repeats must not revive a direction released by a host modal menu.
fn update_game_key(keys: &mut HashSet<GameKey>, key: GameKey, pressed: bool, repeat: bool) -> bool {
    if pressed && repeat {
        false
    } else if pressed {
        keys.insert(key)
    } else {
        keys.remove(&key)
    }
}

/// A host menu owns all input until dismissal. Preserve lifecycle events.
fn discard_queued_game_input(
    keys: &mut HashSet<GameKey>,
    direction: &mut Option<crate::options::Button>,
    queue: &mut VecDeque<Event>,
) {
    keys.clear();
    *direction = None;
    queue.retain(|event| {
        !matches!(
            event,
            Event::TouchesDown(_)
                | Event::TouchesMove(_)
                | Event::TouchesUp(_)
                | Event::ActivateDoor
                | Event::ToggleGameMenu
                | Event::Scroll { .. }
                | Event::TextInput(_)
                | Event::ShowDisplaySettings
        )
    });
}

fn keyboard_horizontal_direction(keys: &HashSet<GameKey>) -> Option<crate::options::Button> {
    let has = |a, b| keys.contains(&a) || keys.contains(&b);
    let left = has(GameKey::A, GameKey::Left);
    let right = has(GameKey::D, GameKey::Right);
    match (left, right) {
        (true, false) => Some(crate::options::Button::DPadLeft),
        (false, true) => Some(crate::options::Button::DPadRight),
        _ => None,
    }
}

/// Touch mode uses the outer 110-pixel strips for walking and the center
/// below y=160 for attacking; D-Pad mode uses the configured button centers.
fn keyboard_button_touch(options: &Options, button: crate::options::Button) -> Option<Coords> {
    if options.keyboard_touch_controls {
        use crate::options::Button;
        match button {
            Button::DPadLeft => return Some((40.0, 290.0)),
            Button::DPadRight => return Some((440.0, 290.0)),
            Button::A => return Some((240.0, 290.0)),
            _ => {},
        }
    }
    options.button_to_touch.get(&button).copied()
}

fn starts_door_action(keys: &HashSet<GameKey>, key: GameKey, pressed: bool) -> bool {
    pressed
        && matches!(key, GameKey::W | GameKey::Up)
        && !keys.contains(&GameKey::W)
        && !keys.contains(&GameKey::Up)
}

#[cfg(test)]
#[test]
fn keyboard_direction_combines_aliases_and_cancels_opposites() {
    let mut keys = HashSet::from([GameKey::A, GameKey::Left]);
    assert_eq!(
        keyboard_horizontal_direction(&keys),
        Some(crate::options::Button::DPadLeft)
    );
    keys.remove(&GameKey::A);
    assert_eq!(
        keyboard_horizontal_direction(&keys),
        Some(crate::options::Button::DPadLeft)
    );
    keys.insert(GameKey::D);
    assert_eq!(keyboard_horizontal_direction(&keys), None);
    keys.remove(&GameKey::Left);
    assert_eq!(
        keyboard_horizontal_direction(&keys),
        Some(crate::options::Button::DPadRight)
    );
}

#[cfg(test)]
#[test]
fn physical_game_keys_do_not_depend_on_layout_keycodes() {
    assert_eq!(game_key(Some(Scancode::W), None), Some(GameKey::W));
    assert_eq!(game_key(Some(Scancode::Up), None), Some(GameKey::Up));
    assert_eq!(game_key(None, Some(Keycode::Up)), Some(GameKey::Up));
    assert_eq!(game_key(Some(Scancode::E), Some(Keycode::E)), None);
    assert_eq!(game_key(Some(Scancode::E), Some(Keycode::W)), None);
    assert_eq!(game_key(Some(Scancode::A), None), Some(GameKey::A));
    assert_eq!(
        game_key(Some(Scancode::Escape), None),
        Some(GameKey::Escape)
    );
}

#[cfg(test)]
#[test]
fn door_action_aliases_fire_once_until_both_released() {
    let mut keys = HashSet::new();
    assert!(starts_door_action(&keys, GameKey::W, true));
    keys.insert(GameKey::W);
    assert!(!starts_door_action(&keys, GameKey::Up, true));
    keys.insert(GameKey::Up);
    keys.remove(&GameKey::W);
    assert!(!starts_door_action(&keys, GameKey::W, true));
    keys.remove(&GameKey::Up);
    assert!(starts_door_action(&keys, GameKey::Up, true));
}

#[derive(Debug)]
pub enum TextInputEvent {
    Text(String),
    Backspace,
    Return,
}

#[derive(Debug)]
pub enum Event {
    /// User requested quit.
    Quit,
    /// OS has informed touchHLE it will soon become inactive.
    /// (iOS `applicationWillResignActive:`, Android `onPause()`)
    AppWillResignActive,
    /// OS has informed touchHLE it will soon terminate.
    /// (iOS `applicationWillTerminate:`, Android `onDestroy()`)
    AppWillTerminate,
    TouchesDown(HashMap<FingerId, Coords>),
    TouchesMove(HashMap<FingerId, Coords>),
    TouchesUp(HashMap<FingerId, Coords>),
    /// Activate the visible door entry in 8-Bit Rebellion.
    ActivateDoor,
    ToggleGameMenu,
    ShowDisplaySettings,
    Scroll {
        cursor: Coords,
        delta: f32,
    },
    #[cfg(feature = "rebellion-test-tools")]
    TestCheat(u8),

    /// User pressed F12, requesting that execution be paused and the debugger
    /// take over.
    EnterDebugger,
    TextInput(TextInputEvent),
}

/// Input from `TOUCHHLE_SCRIPTED_TAPS`, for automated testing.
enum ScriptedInput {
    Touch { x: f32, y: f32, down: bool },
    Text(String),
}

/// Parse `TOUCHHLE_SCRIPTED_TAPS`: `seconds:x,y` taps a point (in game
/// points, released 0.1 seconds later), `seconds:text=abc` types text, and
/// items are separated by `;`.
fn parse_scripted_input() -> VecDeque<(Duration, ScriptedInput)> {
    let mut inputs = Vec::new();
    for item in std::env::var("TOUCHHLE_SCRIPTED_TAPS").unwrap_or_default().split(';') {
        let Some((t, rest)) = item.split_once(':') else {
            continue;
        };
        let Ok(t) = t.trim().parse::<f32>() else {
            continue;
        };
        let at = Duration::from_secs_f32(t);
        if let Some(text) = rest.strip_prefix("text=") {
            inputs.push((at, ScriptedInput::Text(text.to_string())));
        } else if let Some((x, y)) = rest.split_once(',').and_then(|(x, y)| {
            Some((x.trim().parse().ok()?, y.trim().parse().ok()?))
        }) {
            inputs.push((at, ScriptedInput::Touch { x, y, down: true }));
            let up = at + Duration::from_millis(100);
            inputs.push((up, ScriptedInput::Touch { x, y, down: false }));
        }
    }
    inputs.sort_by_key(|&(t, _)| t);
    inputs.into()
}

/// Snapshot the macOS preference once when the window is created.
fn natural_scroll_preference() -> Option<bool> {
    #[cfg(target_os = "macos")]
    {
        let preference = std::process::Command::new("/usr/bin/defaults")
            .args(["read", "-g", "com.apple.swipescrolldirection"])
            .output()
            .ok()
            .filter(|output| output.status.success())
            .and_then(
                |output| match std::str::from_utf8(&output.stdout).ok()?.trim() {
                    "1" | "true" => Some(true),
                    "0" | "false" => Some(false),
                    _ => None,
                },
            );
        match preference {
            Some(enabled) => {
                log!("macOS natural scrolling at startup: {}", enabled);
            }
            None => {
                log!("macOS natural scrolling preference unavailable; using OS event direction");
            }
        }
        preference
    }
    #[cfg(not(target_os = "macos"))]
    {
        Some(false)
    }
}

fn wheel_amount(amount: f32, flipped: bool, natural: Option<bool>) -> f32 {
    // Cocoa's delta already follows the OS setting. Flipped identifies the
    // device-relative sign; normalize it before applying the startup snapshot.
    // If the preference cannot be read, retain the OS-provided delta.
    match natural {
        Some(natural) if flipped != natural => -amount,
        _ => amount,
    }
}

#[cfg(test)]
#[test]
fn wheel_direction_respects_natural_scroll_snapshot_and_os_fallback() {
    for amount in [-1.0, 1.0, -0.25, 0.25] {
        assert_eq!(wheel_amount(amount, false, Some(false)), amount);
        assert_eq!(wheel_amount(amount, true, Some(false)), -amount);
        assert_eq!(wheel_amount(amount, false, Some(true)), -amount);
        assert_eq!(wheel_amount(amount, true, Some(true)), amount);
        assert_eq!(wheel_amount(amount, false, None), amount);
        assert_eq!(wheel_amount(amount, true, None), amount);
    }
}

#[derive(Default)]
struct VideoTouchFilter {
    active: bool,
    skip_requested: bool,
    delivered: HashSet<FingerId>,
    suppressed: HashSet<FingerId>,
}

impl VideoTouchFilter {
    fn filter(&mut self, event: Event) -> Option<Event> {
        match event {
            Event::Scroll { .. } if self.active => None,
            Event::TouchesDown(touches) if self.active => {
                if !touches.is_empty() {
                    self.skip_requested = true;
                    self.suppressed.extend(touches.keys().copied());
                }
                None
            }
            Event::TouchesDown(touches) => {
                for finger in touches.keys() {
                    self.suppressed.remove(finger);
                    self.delivered.insert(*finger);
                }
                Some(Event::TouchesDown(touches))
            }
            Event::TouchesMove(mut touches) => {
                if self.active || !self.suppressed.is_empty() {
                    touches.retain(|finger, _| self.delivered.contains(finger));
                }
                (!touches.is_empty()).then_some(Event::TouchesMove(touches))
            }
            Event::TouchesUp(mut touches) => {
                touches.retain(|finger, _| {
                    self.delivered.remove(finger);
                    !self.suppressed.remove(finger)
                });
                (!touches.is_empty()).then_some(Event::TouchesUp(touches))
            }
            other => Some(other),
        }
    }
}

#[cfg(test)]
mod video_touch_tests {
    use super::{Event, FingerId, HashMap, VideoTouchFilter};

    fn touch_down(finger: FingerId) -> Event {
        Event::TouchesDown(HashMap::from([(finger, (10.0, 20.0))]))
    }
    fn touch_up(finger: FingerId) -> Event {
        Event::TouchesUp(HashMap::from([(finger, (10.0, 20.0))]))
    }

    #[test]
    fn video_tap_is_consumed_without_leaking_release_to_game() {
        for finger in [
            FingerId::Mouse,
            FingerId::Touch(42),
            FingerId::ButtonToTouch(crate::options::Button::A),
        ] {
            let mut filter = VideoTouchFilter::default();
            filter.active = true;
            assert!(filter
                .filter(Event::Scroll {
                    cursor: (10.0, 20.0),
                    delta: -32.0
                })
                .is_none());
            assert!(!filter.skip_requested);
            assert!(filter.filter(touch_down(finger)).is_none());
            assert!(filter.skip_requested);
            filter.active = false;
            assert!(filter.filter(touch_up(finger)).is_none());
            assert!(matches!(
                filter.filter(touch_down(finger)),
                Some(Event::TouchesDown(_))
            ));
        }
    }

    #[test]
    fn release_of_touch_that_started_before_video_reaches_game() {
        let mut filter = VideoTouchFilter::default();
        assert!(filter.filter(touch_down(FingerId::Mouse)).is_some());
        filter.active = true;
        assert!(matches!(
            filter.filter(touch_up(FingerId::Mouse)),
            Some(Event::TouchesUp(_))
        ));
        assert!(!filter.skip_requested);
    }
}

pub enum BatteryState {
    Unknown,
    OnBattery,
    NoBattery,
    Charging,
    Full,
}

pub enum GLVersion {
    /// OpenGL ES 1.1
    GLES11,
    /// OpenGL 2.1 compatibility profile
    GL21Compat,
    /// OpenGL ES 3.0 (WebGL 2). All contexts of this version share one real
    /// context, because a browser canvas only has one.
    GLES30,
}

pub struct GLContext(std::rc::Rc<sdl2::video::GLContext>);

impl GLContext {
    pub fn is_current(&self) -> bool {
        self.0.is_current()
    }
}

/// Keep guest icon artwork inset within the macOS Dock's square canvas.
/// This applies only to the host icon, not to icons drawn by the guest/picker.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn macos_dock_icon(image: Image) -> Image {
    let (width, height) = image.dimensions();
    let side = width.max(height);
    let margin = ((side + 4) / 8).max(1);
    let canvas = side + 2 * margin;
    let left = (canvas - width) / 2;
    let top = (canvas - height) / 2;
    let mut pixels = vec![0; canvas as usize * canvas as usize * 4];
    let row_bytes = width as usize * 4;
    for y in 0..height as usize {
        let source = y * row_bytes;
        let destination = ((y + top as usize) * canvas as usize + left as usize) * 4;
        pixels[destination..destination + row_bytes]
            .copy_from_slice(&image.pixels()[source..source + row_bytes]);
    }
    Image::from_pixel_vec(pixels, (canvas, canvas))
}

fn surface_from_image(image: &Image) -> Surface<'_> {
    let src_pixels = image.pixels();
    let (width, height) = image.dimensions();

    let mut surface = Surface::new(width, height, PixelFormatEnum::RGBA32).unwrap();
    let (width, height) = (width as usize, height as usize);
    let pitch = surface.pitch() as usize;
    surface.with_lock_mut(|dst_pixels| {
        for y in 0..height {
            for x in 0..width {
                for channel in 0..4 {
                    let src_idx = y * width * 4 + x * 4 + channel;
                    let dst_idx = y * pitch + x * 4 + channel;
                    dst_pixels[dst_idx] = src_pixels[src_idx];
                }
            }
        }
    });
    surface
}

pub struct Window {
    _sdl_ctx: sdl2::Sdl,
    video_ctx: sdl2::VideoSubsystem,
    window: sdl2::video::Window,
    event_pump: sdl2::EventPump,
    event_queue: VecDeque<Event>,
    last_polled: Instant,
    /// Input from `TOUCHHLE_SCRIPTED_TAPS`, by time since window creation.
    scripted_input: VecDeque<(Duration, ScriptedInput)>,
    created_at: Instant,
    /// Separate queue for extremely high-priority events (e.g. app about to
    /// terminate).
    high_priority_event: Option<Event>,
    enable_event_polling: bool,
    #[cfg(target_os = "macos")]
    max_height: u32,
    #[cfg(target_os = "macos")]
    viewport_y_offset: u32,
    /// Copy of `fullscreen` on [Options]. Note that this is meaningless when
    /// [Self::rotatable_fullscreen] returns [true].
    fullscreen: bool,
    scale_hack: NonZeroU32,
    landscape_content_layout: bool,
    display_resolution: Option<(u32, u32)>,
    natural_scroll: Option<bool>,
    internal_gl_ins: Option<Box<dyn GLESContext>>,
    /// The real context behind every [GLVersion::GLES30] context.
    shared_gles30_ctx: Option<std::rc::Rc<sdl2::video::GLContext>>,
    splash_image: Option<Image>,
    launch_image_orientation: DeviceOrientation,
    video_frame: Option<Image>,
    video_touch_filter: VideoTouchFilter,
    device_family: DeviceFamily,
    device_orientation: DeviceOrientation,
    controller_ctx: sdl2::GameControllerSubsystem,
    controllers: Vec<sdl2::controller::GameController>,
    dpad_state: DpadState,
    keyboard_keys: HashSet<GameKey>,
    keyboard_direction_active: Option<crate::options::Button>,
    stick_active: bool,
    _sensor_ctx: sdl2::SensorSubsystem,
    accelerometer: Option<sdl2::sensor::Sensor>,
    virtual_cursor_last: Option<(f32, f32, bool, bool)>,
    virtual_cursor_last_unsticky: Option<(f32, f32, Instant)>,
    virtual_accelerometer_last: Option<(f32, f32, bool)>,
    /// Whether or not we are on the "main" environment stack (rather than
    /// a coroutine stack). Checked in various functions to make sure that
    /// certain SDL functions (that call JNI functions) are on the main
    /// stack on Android.
    pub(super) on_main_stack: bool,
}

impl Window {
    /// Returns [true] if touchHLE is running on a device where we should always
    /// display fullscreen, but SDL2 will let us control the orientation, i.e.
    /// Android devices.
    pub fn rotatable_fullscreen() -> bool {
        env::consts::OS == "android"
    }
    pub fn new(
        title: &str,
        icon: Option<Image>,
        launch_image: Option<Image>,
        options: &Options,
    ) -> Window {
        let sdl_ctx = sdl2::init().unwrap();
        let video_ctx = sdl_ctx.video().unwrap();

        // The "hidapi" feature of rust-sdl2 is enabled so that sdl2::sensor
        // is available, but we don't want to enable SDL's HIDAPI controller
        // drivers because they cause duplicated controllers on macOS
        // (https://github.com/libsdl-org/SDL/issues/7479). Once that's fixed,
        // remove this (https://github.com/touchHLE/touchHLE/issues/85).
        sdl2::hint::set("SDL_JOYSTICK_HIDAPI", "0");

        if env::consts::OS == "android" {
            // It's important to set context version BEFORE window creation
            // ref. https://wiki.libsdl.org/SDL2/SDL_GLattr
            let attr = video_ctx.gl_attr();
            attr.set_context_version(1, 1);
            attr.set_context_profile(sdl2::video::GLProfile::GLES);

            // Disable blocking of event loop when app is paused.
            sdl2::hint::set("SDL_ANDROID_BLOCK_ON_PAUSE", "0");
        }

        // Separate mouse and touch events
        sdl2::hint::set("SDL_TOUCH_MOUSE_EVENTS", "0");

        // SDL2 disables the screen saver by default, but iPhone OS enables
        // the idle timer that triggers sleep by default, so we turn it back on
        // here, and then the app can disable it if it wants to.
        video_ctx.enable_screen_saver();

        let scale_hack = options.scale_hack;
        // TODO: some apps specify their orientation in Info.plist, we could use
        // that here.
        let device_family = options.device_family.unwrap_or(DeviceFamily::iPhone);
        let device_orientation = options.initial_orientation;
        let fullscreen = options.fullscreen;

        let mut window = if Self::rotatable_fullscreen() {
            // Without this, SDL will force fullscreen mode to be portrait.
            set_sdl2_orientation(device_orientation);
            let screen_size = video_ctx.display_bounds(0).unwrap().size();
            let (width, height) = rotate_fullscreen_size(device_orientation, screen_size);
            let window = video_ctx
                .window(title, width, height)
                .fullscreen()
                .opengl()
                .build()
                .unwrap();
            window
        } else if fullscreen {
            let (width, height) = video_ctx.display_bounds(0).unwrap().size();
            let window = video_ctx
                .window(title, width, height)
                .fullscreen_desktop()
                .opengl()
                .build()
                .unwrap();
            window
        } else {
            let (width, height) = options.display_resolution.unwrap_or_else(|| {
                size_for_orientation(device_family, device_orientation, scale_hack)
            });
            let window = video_ctx
                .window(title, width, height)
                .position_centered()
                .opengl()
                .build()
                .unwrap();
            window
        };

        if env::consts::OS == "android" {
            // Sanity check
            let gl_attr = video_ctx.gl_attr();
            debug_assert_eq!(gl_attr.context_profile(), sdl2::video::GLProfile::GLES);
            debug_assert_eq!(gl_attr.context_version(), (1, 1));
        }

        if let Some(icon) = icon {
            window.set_icon(surface_from_image(&icon));
        }

        let event_pump = sdl_ctx.event_pump().unwrap();

        let controller_ctx = sdl_ctx.game_controller().unwrap();

        let sensor_ctx = sdl_ctx.sensor().unwrap();
        let mut accelerometer: Option<sdl2::sensor::Sensor> = None;
        if let Ok(num_sensors) = sensor_ctx.num_sensors() {
            for sensor_idx in 0..num_sensors {
                if let Ok(sensor) = sensor_ctx.open(sensor_idx) {
                    if sensor.sensor_type() == sdl2::sensor::SensorType::Accelerometer {
                        log!("Accelerometer detected: {}.", sensor.name());
                        accelerometer = Some(sensor);
                        break;
                    }
                }
            }
        }

        #[cfg(target_os = "macos")]
        let max_height = window.size().1;

        let mut window = Window {
            _sdl_ctx: sdl_ctx,
            video_ctx,
            window,
            event_pump,
            event_queue: VecDeque::new(),
            last_polled: Instant::now() - Duration::from_secs(1),
            scripted_input: parse_scripted_input(),
            created_at: Instant::now(),
            high_priority_event: None,
            enable_event_polling: true,
            #[cfg(target_os = "macos")]
            max_height,
            #[cfg(target_os = "macos")]
            viewport_y_offset: 0,
            fullscreen,
            scale_hack,
            landscape_content_layout: options.landscape_content_layout,
            display_resolution: options.display_resolution,
            natural_scroll: natural_scroll_preference(),
            internal_gl_ins: None,
            shared_gles30_ctx: None,
            splash_image: launch_image,
            launch_image_orientation: device_orientation,
            video_frame: None,
            video_touch_filter: VideoTouchFilter::default(),
            device_family,
            device_orientation,
            controller_ctx,
            controllers: Vec::new(),
            dpad_state: DpadState {
                left: false,
                right: false,
                up: false,
                down: false,
                active: false,
            },
            keyboard_keys: HashSet::new(),
            keyboard_direction_active: None,
            stick_active: false,
            _sensor_ctx: sensor_ctx,
            accelerometer,
            virtual_cursor_last: None,
            virtual_cursor_last_unsticky: None,
            virtual_accelerometer_last: None,
            on_main_stack: true,
        };

        // Set up OpenGL ES context used for splash screen and app UI rendering
        // (see src/frameworks/core_animation/composition.rs). OpenGL ES is used
        // because SDL2 won't let us use more than one graphics API in the same
        // window, and we also need OpenGL ES for the app's own rendering.
        let mut gl_ins = create_gles1_ctx_no_parent_stack(&mut window, options);
        {
            let gl_ctx = gl_ins.make_current(&mut window);
            log!("Driver info: {}", unsafe { gl_ctx.driver_description() });
        }
        window.internal_gl_ins = Some(gl_ins);

        if window.splash_image.is_some() {
            window.display_splash();
        }

        window
    }

    /// Poll for events from the OS. This needs to be done reasonably often
    /// (60Hz is probably fine) so that the host OS doesn't consider touchHLE
    /// to be unresponsive. Note that events are not returned by this function,
    /// since we often need to defer actually handling them.
    ///
    /// Since polling can be quite expensive, this function will skip it if it
    /// was called too recently.
    pub fn poll_for_events(&mut self, options: &Options) {
        assert!(self.on_main_stack);
        let now = Instant::now();
        // poll roughly twice per frame to try to avoid missing frames sometimes
        if now.duration_since(self.last_polled) < Duration::from_secs_f64(1.0 / 120.0) {
            return;
        }
        self.last_polled = now;

        fn transform_input_coords(
            window: &Window,
            (in_x, in_y): (f32, f32),
            independent_of_viewport: bool,
        ) -> (f32, f32) {
            let (vx, vy, vw, vh) = if independent_of_viewport {
                let (width, height) = size_for_orientation(
                    window.device_family,
                    window.device_orientation,
                    NonZeroU32::new(1).unwrap(),
                );
                (0, 0, width, height)
            } else {
                window.viewport()
            };
            // Match the transform used to present the content. In landscape
            // content layout the guest already draws in landscape coordinates.
            let (rotation, output_size) = if window.landscape_content_layout {
                (
                    Matrix::identity(),
                    size_for_orientation(
                        window.device_family,
                        window.device_orientation,
                        NonZeroU32::new(1).unwrap(),
                    ),
                )
            } else {
                (window.rotation_matrix(), window.size_unrotated_unscaled())
            };
            let (out_x, out_y) =
                transform_input_in_viewport((in_x, in_y), (vx, vy, vw, vh), output_size, rotation);
            // Round to match touch precision of official devices.
            (out_x.round(), out_y.round())
        }
        fn transform_virt_accel_coords(window: &Window, (in_x, in_y): (i32, i32)) -> (f32, f32) {
            let (_, _, vw, vh) = window.viewport();
            let out_x = ((in_x as f32 / vw as f32) * 2.0 - 1.0).clamp(-1.0, 1.0);
            let out_y = ((in_y as f32 / vh as f32) * 2.0 - 1.0).clamp(-1.0, 1.0);
            (out_x, out_y)
        }
        fn translate_button(button: sdl2::controller::Button) -> Option<crate::options::Button> {
            match button {
                sdl2::controller::Button::DPadLeft => Some(crate::options::Button::DPadLeft),
                sdl2::controller::Button::DPadUp => Some(crate::options::Button::DPadUp),
                sdl2::controller::Button::DPadRight => Some(crate::options::Button::DPadRight),
                sdl2::controller::Button::DPadDown => Some(crate::options::Button::DPadDown),
                sdl2::controller::Button::Start => Some(crate::options::Button::Start),
                sdl2::controller::Button::A => Some(crate::options::Button::A),
                sdl2::controller::Button::B => Some(crate::options::Button::B),
                sdl2::controller::Button::X => Some(crate::options::Button::X),
                sdl2::controller::Button::Y => Some(crate::options::Button::Y),
                sdl2::controller::Button::LeftShoulder => {
                    Some(crate::options::Button::LeftShoulder)
                }
                _ => None,
            }
        }
        fn finger_absolute_coords(window: &Window, (x, y): (f32, f32)) -> (f32, f32) {
            let (screen_width, screen_height) = window.window.drawable_size();
            (screen_width as f32 * x, screen_height as f32 * y)
        }

        while self
            .scripted_input
            .front()
            .is_some_and(|&(due, _)| self.created_at.elapsed() >= due)
        {
            let (_, input) = self.scripted_input.pop_front().unwrap();
            let event = match input {
                ScriptedInput::Touch { x, y, down } => {
                    let coords = transform_input_coords(self, (x, y), false);
                    log!("Scripted touch {} at {:?}", if down { "down" } else { "up" }, coords);
                    let touches = HashMap::from([(FingerId::Mouse, coords)]);
                    if down {
                        Event::TouchesDown(touches)
                    } else {
                        Event::TouchesUp(touches)
                    }
                }
                ScriptedInput::Text(text) => {
                    log!("Scripted text {:?}", text);
                    Event::TextInput(TextInputEvent::Text(text))
                }
            };
            self.event_queue.push_back(event);
        }

        let mut controller_updated = false;
        // event_pump doesn't have a method to peek on events
        // so, we keep track of an unconsumed one from a previous loop iteration
        // FIXME: use peek_event() from even_subsystem
        let mut previous_event: Option<sdl2::event::Event> = None;
        while self.enable_event_polling {
            use sdl2::event::Event as E;
            let event = if let Some(e) = previous_event.take() {
                match e {
                    E::Unknown { .. } => (),
                    _ => log_dbg!("Consuming previous event: {:?}", e),
                }
                e
            } else if let Some(e) = self.event_pump.poll_event() {
                match e {
                    E::Unknown { .. } => (),
                    _ => log_dbg!("Consuming new event: {:?}", e),
                }
                e
            } else {
                break;
            };

            // Virtual accelerometer
            match event {
                E::MouseButtonDown {
                    x,
                    y,
                    mouse_btn: MouseButton::Right,
                    ..
                } => {
                    let (x, y) = transform_virt_accel_coords(self, (x, y));
                    self.virtual_accelerometer_last = Some((x, y, true));
                }
                E::MouseMotion {
                    x, y, mousestate, ..
                } if mousestate.right() => {
                    let (x, y) = transform_virt_accel_coords(self, (x, y));
                    self.virtual_accelerometer_last = Some((x, y, true));
                }
                E::MouseButtonUp {
                    x,
                    y,
                    mouse_btn: MouseButton::Right,
                    ..
                } => {
                    let (x, y) = transform_virt_accel_coords(self, (x, y));
                    self.virtual_accelerometer_last = Some((x, y, false));
                }
                _ => {}
            }

            let mut keyboard_release = None;
            let mapped_event = match event {
                E::Quit { .. } => Event::Quit,
                E::MouseButtonDown {
                    x,
                    y,
                    mouse_btn: MouseButton::Left,
                    ..
                } => {
                    let coords = transform_input_coords(self, (x as f32, y as f32), false);
                    log_dbg!("MouseButtonDown x {}, y {}, coords {:?}", x, y, coords);
                    Event::TouchesDown(HashMap::from([(FingerId::Mouse, coords)]))
                }
                E::MouseMotion {
                    x, y, mousestate, ..
                } if mousestate.left() => {
                    let coords = transform_input_coords(self, (x as f32, y as f32), false);
                    log_dbg!("MouseMotion x {}, y {}, coords {:?}", x, y, coords);
                    Event::TouchesMove(HashMap::from([(FingerId::Mouse, coords)]))
                }
                E::MouseButtonUp {
                    x,
                    y,
                    mouse_btn: MouseButton::Left,
                    ..
                } => {
                    let coords = transform_input_coords(self, (x as f32, y as f32), false);
                    log_dbg!("MouseButtonUp x {}, y {}, coords {:?}", x, y, coords);
                    Event::TouchesUp(HashMap::from([(FingerId::Mouse, coords)]))
                }
                E::MouseWheel {
                    y,
                    precise_y,
                    direction,
                    mouse_x,
                    mouse_y,
                    ..
                } if options.keyboard_game_controls => {
                    if self.event_pump.mouse_state().left() {
                        continue;
                    }
                    let amount = if precise_y == 0.0 {
                        y as f32
                    } else {
                        precise_y
                    };
                    let amount = wheel_amount(
                        amount,
                        direction == sdl2::mouse::MouseWheelDirection::Flipped,
                        self.natural_scroll,
                    );
                    if !amount.is_finite() || amount == 0.0 {
                        continue;
                    }
                    let cursor =
                        transform_input_coords(self, (mouse_x as f32, mouse_y as f32), false);
                    Event::Scroll {
                        cursor,
                        delta: amount * 32.0,
                    }
                }
                E::ControllerDeviceAdded { which, .. } => {
                    self.controller_added(which);
                    continue;
                }
                E::ControllerDeviceRemoved { which, .. } => {
                    self.controller_removed(which);
                    continue;
                }
                // Note that accelerometer simulation with analog sticks is
                // handled with polling, rather than being event-based.
                E::ControllerButtonUp { button, .. } | E::ControllerButtonDown { button, .. } => {
                    controller_updated = true;
                    let Some(button) = translate_button(button) else {
                        continue;
                    };
                    // Called whenever a DPad direction is pressed or released
                    if (button == crate::options::Button::DPadLeft
                        || button == crate::options::Button::DPadUp
                        || button == crate::options::Button::DPadRight
                        || button == crate::options::Button::DPadDown)
                        && options.dpad_to_touch.is_some()
                    {
                        let Some((x, y, w, h)) = options.dpad_to_touch else {
                            unreachable!();
                        };

                        // Update held state
                        let pressed = matches!(event, E::ControllerButtonDown { .. });
                        match button {
                            crate::options::Button::DPadLeft => self.dpad_state.left = pressed,
                            crate::options::Button::DPadRight => self.dpad_state.right = pressed,
                            crate::options::Button::DPadUp => self.dpad_state.up = pressed,
                            crate::options::Button::DPadDown => self.dpad_state.down = pressed,
                            _ => unreachable!(),
                        }

                        // Compute center
                        let cx = x + w * 0.5;
                        let cy = y + h * 0.5;

                        // Compute combined delta
                        let mut dx = 0.0;
                        let mut dy = 0.0;

                        if self.dpad_state.left {
                            dx -= 0.5 * w;
                        }
                        if self.dpad_state.right {
                            dx += 0.5 * w;
                        }
                        if self.dpad_state.up {
                            dy -= 0.5 * h;
                        }
                        if self.dpad_state.down {
                            dy += 0.5 * h;
                        }

                        // Final coords: center + movement
                        let coords = transform_input_coords(self, (cx + dx, cy + dy), true);

                        // Send TouchDown if any dpad is held, TouchUp if none
                        let any_held = self.dpad_state.left
                            || self.dpad_state.right
                            || self.dpad_state.up
                            || self.dpad_state.down;

                        if !self.dpad_state.active && any_held {
                            // New touch
                            self.dpad_state.active = true;
                            Event::TouchesDown(HashMap::from([(FingerId::DpadToTouch, coords)]))
                        } else if self.dpad_state.active && any_held {
                            // Move existing touch
                            Event::TouchesMove(HashMap::from([(FingerId::DpadToTouch, coords)]))
                        } else if self.dpad_state.active && !any_held {
                            // Release touch
                            self.dpad_state.active = false;
                            Event::TouchesUp(HashMap::from([(FingerId::DpadToTouch, coords)]))
                        } else {
                            continue;
                        }
                    } else {
                        let Some(&(x, y)) = options.button_to_touch.get(&button) else {
                            continue;
                        };
                        match event {
                            E::ControllerButtonUp { .. } => {
                                let coords = transform_input_coords(self, (x, y), true);
                                Event::TouchesUp(HashMap::from([(
                                    FingerId::ButtonToTouch(button),
                                    coords,
                                )]))
                            }
                            E::ControllerButtonDown { .. } => {
                                let coords = transform_input_coords(self, (x, y), true);
                                Event::TouchesDown(HashMap::from([(
                                    FingerId::ButtonToTouch(button),
                                    coords,
                                )]))
                            }
                            _ => unreachable!(),
                        }
                    }
                }
                E::ControllerAxisMotion { axis, .. } => {
                    controller_updated = true;
                    let Some((x, y, w, h)) = options.stick_to_touch else {
                        continue;
                    };
                    if axis == sdl2::controller::Axis::LeftX
                        || axis == sdl2::controller::Axis::LeftY
                    {
                        let (stick_x, stick_y, _) = self.get_controller_stick(options, true);
                        let coords = transform_input_coords(
                            self,
                            (
                                x + ((stick_x + 1.0) / 2.0) * w,
                                y + ((stick_y + 1.0) / 2.0) * h,
                            ),
                            true,
                        );
                        if stick_x.abs() < options.deadzone && stick_y.abs() < options.deadzone {
                            if !self.stick_active {
                                // Ignore deadzone events when stick is inactive
                                continue;
                            } else {
                                // Release touch when stick returns to deadzone
                                self.stick_active = false;
                                Event::TouchesUp(HashMap::from([(FingerId::StickToTouch, coords)]))
                            }
                        } else if !self.stick_active {
                            // New touch
                            self.stick_active = true;
                            Event::TouchesDown(HashMap::from([(FingerId::StickToTouch, coords)]))
                        } else {
                            // Move existing touch
                            Event::TouchesMove(HashMap::from([(FingerId::StickToTouch, coords)]))
                        }
                    } else {
                        continue;
                    }
                }
                E::AppWillEnterBackground { .. } => {
                    log!("Received app-will-resign-active event.");
                    assert!(self.high_priority_event.is_none());
                    self.high_priority_event = Some(Event::AppWillResignActive);
                    // For some reason, if we don't pause event polling, we will
                    // never finish handling the event.
                    // TODO: Add a mechanism for re-enabling polling, if at some
                    // point we support returning touchHLE to the foreground.
                    self.enable_event_polling = false;
                    continue;
                }
                E::AppTerminating { .. } => {
                    log!("Received app-will-terminate event.");
                    assert!(self.high_priority_event.is_none());
                    self.high_priority_event = Some(Event::AppWillTerminate);
                    self.enable_event_polling = false;
                    continue;
                }
                E::FingerUp {
                    timestamp,
                    finger_id,
                    x,
                    y,
                    ..
                }
                | E::FingerMotion {
                    timestamp,
                    finger_id,
                    x,
                    y,
                    ..
                }
                | E::FingerDown {
                    timestamp,
                    finger_id,
                    x,
                    y,
                    ..
                } => {
                    log_dbg!("Starting multi-touch for {:?}", event);
                    // To implement multi-touch we accumulate here same touch
                    // events at the same timestamp. This is consistent with
                    // UIKit, but could be broken if events come out of order.
                    // (in worst case we separate multi-touches in several ones)
                    // TODO: handle out of order touches
                    let curr_timestamp = timestamp;
                    let abs_coords = finger_absolute_coords(self, (x, y));
                    let coords = transform_input_coords(self, abs_coords, false);
                    if options.keyboard_game_controls && !matches!(&event, E::FingerMotion { .. }) {
                        log!("8-Bit Rebellion SDL finger {event:?}: guest={coords:?}");
                    }
                    log_dbg!("Finger event x {}, y {}, coords {:?}", x, y, coords);
                    let mut map = HashMap::from([(FingerId::Touch(finger_id), coords)]);
                    while let Some(next) = self.event_pump.poll_event() {
                        match next {
                            E::Unknown { .. } => (),
                            _ => log_dbg!("Next possible multi-touch event: {:?}", next),
                        }
                        match next {
                            E::FingerUp {
                                timestamp,
                                finger_id,
                                x,
                                y,
                                ..
                            }
                            | E::FingerMotion {
                                timestamp,
                                finger_id,
                                x,
                                y,
                                ..
                            }
                            | E::FingerDown {
                                timestamp,
                                finger_id,
                                x,
                                y,
                                ..
                            } if timestamp == curr_timestamp && next.is_same_kind_as(&event) => {
                                let abs_coords = finger_absolute_coords(self, (x, y));
                                let coords = transform_input_coords(self, abs_coords, false);
                                map.insert(FingerId::Touch(finger_id), coords);
                            }
                            E::MultiGesture { timestamp, .. } if timestamp == curr_timestamp => {
                                // TODO: handle gestures
                                continue;
                            }
                            _ => {
                                // event_pump doesn't have a method to peek on
                                // events, so we keep track of an unconsumed
                                // one from a previous loop iteration
                                assert!(previous_event.is_none());
                                previous_event = Some(next);
                                break;
                            }
                        }
                    }
                    log_dbg!("Finishing multi-touch for {:?} with {:?}", event, map);
                    match event {
                        E::FingerUp { .. } => Event::TouchesUp(map),
                        E::FingerMotion { .. } => Event::TouchesMove(map),
                        E::FingerDown { .. } => Event::TouchesDown(map),
                        _ => unreachable!(),
                    }
                }
                E::KeyDown {
                    keycode: Some(sdl2::keyboard::Keycode::F12),
                    ..
                } => {
                    // Log this so you can tell when touchHLE has received
                    // the event but it's stuck in the queue.
                    echo!("F12 pressed, EnterDebugger event queued.");
                    Event::EnterDebugger
                }
                E::Window {
                    win_event: sdl2::event::WindowEvent::FocusLost,
                    ..
                } if options.keyboard_game_controls => {
                    let mut touches = HashMap::new();
                    if let Some(old) = self.keyboard_direction_active.take() {
                        if let Some((x, y)) = keyboard_button_touch(options, old) {
                            touches.insert(
                                FingerId::KeyboardDirection,
                                transform_input_coords(self, (x, y), true),
                            );
                        }
                    }
                    if self.keyboard_keys.contains(&GameKey::Space) {
                        if let Some((x, y)) =
                            keyboard_button_touch(options, crate::options::Button::A)
                        {
                            touches.insert(
                                FingerId::KeyboardAttack,
                                transform_input_coords(self, (x, y), true),
                            );
                        }
                    }
                    self.keyboard_keys.clear();
                    if touches.is_empty() {
                        continue;
                    }
                    Event::TouchesUp(touches)
                }
                E::KeyDown {
                    scancode: Some(sdl2::keyboard::Scancode::F9),
                    repeat: false,
                    ..
                } if options.display_service => {
                    Event::ShowDisplaySettings
                }
                #[cfg(feature = "rebellion-test-tools")]
                E::KeyDown {
                    scancode: Some(key),
                    repeat: false,
                    ..
                } if matches!(
                    key,
                    sdl2::keyboard::Scancode::F5
                        | sdl2::keyboard::Scancode::F6
                        | sdl2::keyboard::Scancode::F7
                ) =>
                {
                    Event::TestCheat(match key {
                        sdl2::keyboard::Scancode::F5 => 1,
                        sdl2::keyboard::Scancode::F6 => 2,
                        _ => 3,
                    })
                }
                #[cfg(feature = "rebellion-test-tools")]
                E::KeyDown {
                    scancode: Some(sdl2::keyboard::Scancode::F8),
                    repeat: false,
                    ..
                } => {
                    let Some(action) = show_rebellion_test_menu(&self.window) else {
                        continue;
                    };
                    Event::TestCheat(action)
                }
                E::KeyDown {
                    keycode, scancode, ..
                }
                | E::KeyUp {
                    keycode, scancode, ..
                } if options.keyboard_game_controls && game_key(scancode, keycode).is_some() => {
                    let key = game_key(scancode, keycode).unwrap();
                    let pressed = matches!(event, E::KeyDown { .. });
                    let start_door_action = starts_door_action(&self.keyboard_keys, key, pressed);
                    let repeat = matches!(event, E::KeyDown { repeat: true, .. });
                    let changed = update_game_key(&mut self.keyboard_keys, key, pressed, repeat);
                    if !changed {
                        continue; // SDL key repeat must not create extra touches.
                    }

                    log!(
                        "8-Bit Rebellion key {:?} {}: direction={:?}, attack_held={}",
                        key,
                        if pressed { "down" } else { "up" },
                        keyboard_horizontal_direction(&self.keyboard_keys),
                        self.keyboard_keys.contains(&GameKey::Space),
                    );

                    if key == GameKey::Space {
                        let Some((x, y)) = keyboard_button_touch(options, crate::options::Button::A)
                        else {
                            continue;
                        };
                        let coords = transform_input_coords(self, (x, y), true);
                        if pressed {
                            Event::TouchesDown(HashMap::from([(FingerId::KeyboardAttack, coords)]))
                        } else {
                            Event::TouchesUp(HashMap::from([(FingerId::KeyboardAttack, coords)]))
                        }
                    } else if key == GameKey::W || key == GameKey::Up {
                        if start_door_action {
                            Event::ActivateDoor
                        } else {
                            continue;
                        }
                    } else if key == GameKey::Escape {
                        if pressed {
                            Event::ToggleGameMenu
                        } else {
                            continue;
                        }
                    } else {
                        let new_direction = keyboard_horizontal_direction(&self.keyboard_keys);
                        let old_direction = self.keyboard_direction_active;
                        if old_direction == new_direction {
                            continue;
                        }
                        self.keyboard_direction_active = new_direction;
                        let touch_for = |this: &Window, button| {
                            keyboard_button_touch(options, button)
                                .map(|(x, y)| transform_input_coords(this, (x, y), true))
                        };
                        if let Some(old) = old_direction {
                            if let Some(coords) = touch_for(self, old) {
                                keyboard_release = Some(Event::TouchesUp(HashMap::from([(
                                    FingerId::KeyboardDirection,
                                    coords,
                                )])));
                            }
                        }
                        if let Some(new) = new_direction {
                            let Some(coords) = touch_for(self, new) else {
                                continue;
                            };
                            Event::TouchesDown(HashMap::from([(
                                FingerId::KeyboardDirection,
                                coords,
                            )]))
                        } else if let Some(release) = keyboard_release.take() {
                            release
                        } else {
                            continue;
                        }
                    }
                }
                E::KeyDown {
                    keycode: Some(sdl2::keyboard::Keycode::Backspace),
                    ..
                } => {
                    log_dbg!("SDL TextInput Backspace");
                    Event::TextInput(TextInputEvent::Backspace)
                }
                E::KeyDown {
                    keycode: Some(sdl2::keyboard::Keycode::Return),
                    ..
                } => {
                    log_dbg!("SDL TextInput Return");
                    Event::TextInput(TextInputEvent::Return)
                }
                E::TextInput { text, .. } => {
                    log_dbg!("SDL TextInput {}", text);
                    Event::TextInput(TextInputEvent::Text(text))
                }
                _ => continue,
            };
            if let Some(release) = keyboard_release {
                self.event_queue.push_back(release);
            }
            self.event_queue.push_back(mapped_event);
        }

        if controller_updated {
            let (new_x, new_y, pressed, pressed_changed, moved) =
                self.update_virtual_cursor(options);
            self.event_queue
                .push_back(match (pressed, pressed_changed, moved) {
                    (true, true, _) => {
                        let coords = transform_input_coords(self, (new_x, new_y), false);
                        Event::TouchesDown(HashMap::from([(FingerId::VirtualCursor, coords)]))
                    }
                    (false, true, _) => {
                        let coords = transform_input_coords(self, (new_x, new_y), false);
                        Event::TouchesUp(HashMap::from([(FingerId::VirtualCursor, coords)]))
                    }
                    (true, _, true) => {
                        let coords = transform_input_coords(self, (new_x, new_y), false);
                        Event::TouchesMove(HashMap::from([(FingerId::VirtualCursor, coords)]))
                    }
                    _ => return,
                });
        }
    }

    /// Discard input captured by a native modal menu, including already mapped
    /// touches. UIKit must end its active touches separately before opening it.
    pub(crate) fn discard_modal_input(&mut self) {
        assert!(self.on_main_stack);
        self.event_pump.pump_events();
        let events = self._sdl_ctx.event().unwrap();
        use sdl2::event::EventType as T;
        for kind in [
            T::KeyDown,
            T::KeyUp,
            T::TextInput,
            T::TextEditing,
            T::MouseMotion,
            T::MouseButtonDown,
            T::MouseButtonUp,
            T::MouseWheel,
            T::FingerDown,
            T::FingerUp,
            T::FingerMotion,
        ] {
            events.flush_event(kind);
        }
        discard_queued_game_input(
            &mut self.keyboard_keys,
            &mut self.keyboard_direction_active,
            &mut self.event_queue,
        );
    }

    pub(crate) fn held_keyboard_direction(&self) -> Option<crate::options::Button> {
        self.keyboard_direction_active
    }

    /// Pop an event from the queue (in FIFO order, except for high priority
    /// events)
    pub fn pop_event(&mut self) -> Option<Event> {
        loop {
            let event = self
                .high_priority_event
                .take()
                .or_else(|| self.event_queue.pop_front())?;
            if let Some(event) = self.video_touch_filter.filter(event) {
                return Some(event);
            }
        }
    }

    fn controller_added(&mut self, joystick_idx: u32) {
        let Ok(controller) = self.controller_ctx.open(joystick_idx) else {
            log!("Warning: A new controller was connected, but it couldn't be accessed!");
            return;
        };

        let controller_name = controller.name();
        if env::consts::OS == "android" && controller_name.starts_with("uinput-") {
            log!("ignoring fingerprint device: {}", controller_name);
            return;
        }
        log!(
            "New controller connected: {}. Left stick = device tilt. Right stick = touch input (press the stick or shoulder button to tap/hold).",
            controller_name
        );
        self.controllers.push(controller);
    }
    fn controller_removed(&mut self, instance_id: u32) {
        let Some(idx) = self
            .controllers
            .iter()
            .position(|controller| controller.instance_id() == instance_id)
        else {
            return;
        };
        let controller = self.controllers.remove(idx);
        log!("Warning: Controller disconnected: {}", controller.name());
    }
    pub fn print_accelerometer_notice(&self, options: &Options) {
        log!("This app uses the accelerometer.");

        if !self.controllers.is_empty() && options.analog_stick_tilt_controls {
            log!("Your connected controller's left analog stick will be used for accelerometer simulation.");
            if self.accelerometer.is_some() {
                log!("Disconnect the controller if you want to use your device's accelerometer.");
            }
        } else if self.accelerometer.is_some() {
            log!("Your device's accelerometer will be used for accelerometer simulation.");
            if options.analog_stick_tilt_controls {
                log!("Connect a controller if you would prefer to use an analog stick.");
            }
        } else if self.controllers.is_empty() && options.analog_stick_tilt_controls {
            log!("Connect a controller to get accelerometer simulation.");
        }

        if self.accelerometer.is_none() {
            log!(
                "You can {}hold right click and move the cursor to simulate the accelerometer.",
                if options.analog_stick_tilt_controls {
                    "also "
                } else {
                    ""
                }
            );
        }
    }

    /// Get the real or simulated accelerometer output.
    /// See also [crate::frameworks::uikit::ui_accelerometer].
    pub fn get_acceleration(&self, options: &Options) -> (f32, f32, f32) {
        if self.controllers.is_empty() || !options.analog_stick_tilt_controls {
            if let Some(ref accelerometer) = self.accelerometer {
                let data = accelerometer.get_data().unwrap();
                let sdl2::sensor::SensorData::Accel(data) = data else {
                    panic!();
                };
                let [x, y, z] = data;
                // UIAcceleration reports acceleration towards gravity, but SDL2
                // reports acceleration away from gravity.
                let (x, y, z) = (-x, -y, -z);
                // UIAcceleration reports acceleration in units of g-force, but
                // SDL2 reports acceleration in units of m/s^2.
                let gravity: f32 = 9.80665; // SDL_STANDARD_GRAVITY
                let (x, y, z) = (x / gravity, y / gravity, z / gravity);
                return (x, y, z);
            }
        }

        let (x, y) = if self
            .virtual_accelerometer_last
            .is_some_and(|(_x, _y, right_click_hold)| right_click_hold)
        {
            self.virtual_accelerometer_last
                .map(|(x, y, _right_click_hold)| (x, y))
                .unwrap()
        } else {
            // Get left analog stick input. The range is [-1, 1] on each axis.
            let (x, y, _) = self.get_controller_stick(options, true);
            (x, y)
        };

        // Correct for window rotation
        let [x, y] = self.rotation_matrix().inverse().unwrap().transform([x, y]);
        let (x, y) = (x.clamp(-1.0, 1.0), y.clamp(-1.0, 1.0)); // just in case

        // Let's simulate tilting the device based on the analog stick inputs.
        //
        // If an iPhone is lying flat on its back, level with the ground, and it
        // is on Earth, the accelerometer will report approximately (0, 0, -1).
        // The acceleration x and y axes are aligned with the screen's x and y
        // axes. +x points to the right of the screen, +y points to the top of
        // the screen, and +z points away from the screen. In the example
        // scenario, the z axis is parallel to gravity.

        let gravity: [f32; 3] = [0.0, 0.0, -1.0];

        let neutral_x = options.x_tilt_offset.to_radians();
        let neutral_y = options.y_tilt_offset.to_radians();
        let x_rotation_range = options.x_tilt_range.to_radians() / 2.0;
        let y_rotation_range = options.y_tilt_range.to_radians() / 2.0;
        // (x, y) are swapped because the controller Y axis usually corresponds
        // to forward/backward movement, but rotating about the Y axis means
        // tilting the device left/right.
        let x_rotation = neutral_x - x_rotation_range * y;
        let y_rotation = neutral_y - y_rotation_range * x;
        let matrix =
            Matrix::<3>::y_rotation(y_rotation).multiply(&Matrix::<3>::x_rotation(x_rotation));
        let [x, y, z] = matrix.transform(gravity);

        (x, y, z)
    }

    /// For use when redrawing the screen: Get the cached on-screen position and
    /// press state of the analog stick-controlled virtual cursor, if it is
    /// visible.
    pub fn virtual_cursor_visible_at(&self) -> Option<(f32, f32, bool)> {
        let (x, y, pressed, visible) = self.virtual_cursor_last?;
        if visible {
            // When stickyness is in use, the visual cursor movement appears
            // uncomfortably choppy. Showing the un-sticky position is a bit
            // misleading but it *feels* better, and it is documented.
            if let Some((x_unsticky, y_unsticky, _time)) = self.virtual_cursor_last_unsticky {
                Some((x_unsticky, y_unsticky, pressed))
            } else {
                Some((x, y, pressed))
            }
        } else {
            None
        }
    }

    /// Update the virtual cursor's position, click state and visibility, then
    /// return the new position, pressed state, whether the press state changed
    /// and whether the cursor moved.
    fn update_virtual_cursor(&mut self, options: &Options) -> (f32, f32, bool, bool, bool) {
        // Get right analog stick input. The range is [-1, 1] on each axis.
        let (x, y, pressed) = self.get_controller_stick(options, false);

        // The cursor is intended to only show up once you move the analog stick
        // out of its deadzone, or while the button is held.
        let visible = pressed || x != 0.0 || y != 0.0;

        // Though the analog stick output fits within a square, its actual range
        // is usually a circle enclosed by the square. So we need to cut out the
        // rectangular shape of the screen from that circle within the square.
        let (vx, vy, vw, vh) = self.viewport();
        let (vx, vy, vw, vh) = (vx as f32, vy as f32, vw as f32, vh as f32);

        let (x, y) = {
            // Use Pythagoras's theorem to find the largest size the rectangle
            // can have within the circle.
            let ratio = vw / vh;
            let rect_height = (ratio * ratio + 1.0).powf(-0.5);
            let rect_width = ratio * rect_height;

            let x_abs = x.abs().min(rect_width) / rect_width;
            let y_abs = y.abs().min(rect_height) / rect_height;
            (x_abs.copysign(x), y_abs.copysign(y))
        };

        // Convert to on-screen window co-ordinates
        let x = (x / 2.0 + 0.5) * vw + vx;
        let y = (y / 2.0 + 0.5) * vh + vy;

        let (old_x, old_y, old_pressed, _old_visible) =
            self.virtual_cursor_last.unwrap_or_default();

        let (x, y) = if let Some((smoothing_strength, sticky_radius)) =
            options.stabilize_virtual_cursor
        {
            let new_time = Instant::now();

            let (old_x_unsticky, old_y_unsticky, old_time) = self
                .virtual_cursor_last_unsticky
                .unwrap_or((0.0, 0.0, new_time));

            let delta_t = new_time.saturating_duration_since(old_time).as_secs_f32();

            // Apply a feedback-based smoothing with exponential decay, to try
            // to dampen shakiness in the stick movement.

            let smooth = |old: f32, new: f32| -> f32 {
                if smoothing_strength != 0.0 {
                    let lerp_factor = 1.0 - (0.5_f32).powf(delta_t * (1.0 / smoothing_strength));
                    old + (new - old) * lerp_factor
                } else {
                    new
                }
            };

            let new_x_unsticky = smooth(old_x_unsticky, x);
            let new_y_unsticky = smooth(old_y_unsticky, y);

            self.virtual_cursor_last_unsticky = Some((new_x_unsticky, new_y_unsticky, new_time));

            // Make the reported position "sticky" within a certain radius, i.e.
            // if the new position's distance from the old one is within the
            // radius, report no change in position.

            if (new_x_unsticky - old_x).hypot(new_y_unsticky - old_y) < sticky_radius {
                (old_x, old_y)
            } else {
                (new_x_unsticky, new_y_unsticky)
            }
        } else {
            (x, y)
        };

        self.virtual_cursor_last = Some((x, y, pressed, visible));

        (
            x,
            y,
            pressed,
            pressed != old_pressed,
            x != old_x || y != old_y,
        )
    }

    /// Get the summed X and Y positions and button state of the left or right
    /// analog stick of the game controllers. Each axis value is in the range
    /// [-1, 1].
    fn get_controller_stick(&self, options: &Options, left: bool) -> (f32, f32, bool) {
        fn convert_axis(axis: i16, deadzone: f32) -> f32 {
            assert!(deadzone >= 0.0);
            let axis = ((axis as f32) / (i16::MAX as f32)).clamp(-1.0, 1.0);
            let abs_axis = (axis.abs().max(deadzone) - deadzone) / (1.0 - deadzone);
            abs_axis.copysign(axis)
        }

        let (mut x, mut y) = (0.0, 0.0);
        let mut pressed = false;
        for controller in &self.controllers {
            use sdl2::controller::{Axis, Button};
            let (x_axis, y_axis, button1, button2) = if left {
                (
                    Axis::LeftX,
                    Axis::LeftY,
                    Button::LeftStick,
                    Button::LeftShoulder,
                )
            } else {
                (
                    Axis::RightX,
                    Axis::RightY,
                    Button::RightStick,
                    Button::RightShoulder,
                )
            };
            x += convert_axis(controller.axis(x_axis), options.deadzone);
            y += convert_axis(controller.axis(y_axis), options.deadzone);
            pressed |= controller.button(button1);
            pressed |= controller.button(button2);
        }
        let (x, y) = (x.clamp(-1.0, 1.0), y.clamp(-1.0, 1.0));

        (x, y, pressed)
    }

    pub fn create_gl_context(&mut self, version: GLVersion) -> Result<GLContext, String> {
        let attr = self.video_ctx.gl_attr();
        match version {
            GLVersion::GLES11 => {
                attr.set_context_version(1, 1);
                attr.set_context_profile(sdl2::video::GLProfile::GLES);
            }
            GLVersion::GL21Compat => {
                attr.set_context_version(2, 1);
                attr.set_context_profile(sdl2::video::GLProfile::Compatibility);
            }
            GLVersion::GLES30 => {
                if let Some(ctx) = &self.shared_gles30_ctx {
                    return Ok(GLContext(ctx.clone()));
                }
                attr.set_context_version(3, 0);
                attr.set_context_profile(sdl2::video::GLProfile::GLES);
            }
        }

        let gl_ctx = std::rc::Rc::new(self.window.gl_create_context()?);
        if let GLVersion::GLES30 = version {
            self.shared_gles30_ctx = Some(gl_ctx.clone());
        }

        Ok(GLContext(gl_ctx))
    }

    pub fn gl_get_proc_address(&self, procname: &str) -> *const std::ffi::c_void {
        // For some reason, rust-sdl2 uses *const (), but () is not meant to be
        // used for void pointees (just void results), so let's fix that.
        self.video_ctx.gl_get_proc_address(procname) as *const _
    }

    pub fn set_share_with_current_context(&self, value: bool) {
        self.video_ctx
            .gl_attr()
            .set_share_with_current_context(value)
    }

    pub unsafe fn make_gl_context_current(&self, gl_ctx: &GLContext) {
        self.window.gl_make_current(&gl_ctx.0).unwrap();
    }

    /// Make the internal OpenGL ES context (for splash screen and UI rendering)
    /// current.
    #[must_use]
    pub fn make_internal_gl_ctx_current<'win>(&'win mut self) -> Box<dyn GLES + 'win> {
        // The invariant is held up here - since the instance we return is
        // bound to the lifetime of window, it can't outlive the internal GL
        // context and can't outlive the window.
        let gl_ins = unsafe {
            self.internal_gl_ins
                .as_mut()
                .unwrap()
                .make_current_unchecked_for_window(
                    &mut |gl_ctx| self.window.gl_make_current(&gl_ctx.0).unwrap(),
                    &mut |s| self.video_ctx.gl_get_proc_address(s) as *const _,
                )
        };
        gl_ins
    }

    fn display_splash(&mut self) {
        assert!(self.splash_image.is_some());

        // OpenGL ES expects bottom-to-top row order for image data, but our
        // image data will be top-to-bottom. A reflection transform compensates.
        let rotation = if self.landscape_content_layout {
            // The compositor rotates vertices, while this path rotates texture
            // coordinates: the matching launch orientation is its inverse.
            orientation_rotation(self.launch_image_orientation).transpose()
        } else {
            self.rotation_matrix()
        };
        let matrix = rotation.multiply(&Matrix::y_flip());
        let (vx, vy, vw, vh) = self.viewport();
        let viewport = (vx, vy + self.viewport_y_offset(), vw, vh);

        let image = self.splash_image.as_ref().unwrap();
        unsafe {
            let mut gl_ctx = self
                .internal_gl_ins
                .as_mut()
                .unwrap()
                .make_current_unchecked_for_window(
                    &mut |gl_ctx| self.window.gl_make_current(&gl_ctx.0).unwrap(),
                    &mut |s| self.video_ctx.gl_get_proc_address(s) as *const _,
                );

            use crate::gles::gles11_raw as gles11; // constants only

            let mut texture = 0;
            gl_ctx.GenTextures(1, &mut texture);
            gl_ctx.BindTexture(gles11::TEXTURE_2D, texture);
            let (width, height) = image.dimensions();
            gl_ctx.TexImage2D(
                gles11::TEXTURE_2D,
                0,
                gles11::RGBA as _,
                width as _,
                height as _,
                0,
                gles11::RGBA,
                gles11::UNSIGNED_BYTE,
                image.pixels().as_ptr() as *const _,
            );
            gl_ctx.TexParameteri(
                gles11::TEXTURE_2D,
                gles11::TEXTURE_MIN_FILTER,
                gles11::LINEAR as _,
            );
            gl_ctx.TexParameteri(
                gles11::TEXTURE_2D,
                gles11::TEXTURE_MAG_FILTER,
                gles11::LINEAR as _,
            );

            present_frame(
                gl_ctx.as_mut(),
                viewport,
                matrix,
                /* virtual_cursor_visible_at: */ None,
            );

            gl_ctx.DeleteTextures(1, &texture);
        };

        self.window.gl_swap_window();

        // hold onto GL context so the image doesn't disappear, and hold
        // onto image so we can rotate later if necessary
    }

    /// Swap front-buffer and back-buffer so the result of OpenGL rendering is
    /// presented.
    pub fn set_video_frame(&mut self, frame: Option<Image>) {
        self.video_frame = frame;
    }

    pub fn is_video_active(&self) -> bool {
        self.video_touch_filter.active
    }

    pub fn set_video_touch_active(&mut self, active: bool) {
        self.video_touch_filter.active = active;
        if active {
            self.video_touch_filter.skip_requested = false;
        }
    }

    pub fn take_video_skip_request(&mut self) -> bool {
        std::mem::take(&mut self.video_touch_filter.skip_requested)
    }

    fn draw_video_frame(&mut self) {
        let Some(image) = self.video_frame.as_ref() else {
            return;
        };
        let (screen_width, screen_height) = self.window.drawable_size();
        let (width, height) = image.dimensions();
        let scale = (screen_width as f32 / width as f32).min(screen_height as f32 / height as f32);
        let output_width = (width as f32 * scale).round() as u32;
        let output_height = (height as f32 * scale).round() as u32;
        let viewport = (
            (screen_width - output_width) / 2,
            (screen_height - output_height) / 2,
            output_width,
            output_height,
        );

        unsafe {
            use crate::gles::gles11_raw as gles11;

            let mut gl_ctx = self
                .internal_gl_ins
                .as_mut()
                .unwrap()
                .make_current_unchecked_for_window(
                    &mut |ctx| self.window.gl_make_current(&ctx.0).unwrap(),
                    &mut |name| self.video_ctx.gl_get_proc_address(name) as *const _,
                );
            gl_ctx.Disable(gles11::SCISSOR_TEST);
            gl_ctx.Disable(gles11::BLEND);
            gl_ctx.Disable(gles11::DEPTH_TEST);
            gl_ctx.Disable(gles11::STENCIL_TEST);
            gl_ctx.Disable(gles11::ALPHA_TEST);
            gl_ctx.Disable(gles11::CULL_FACE);
            gl_ctx.Disable(gles11::LIGHTING);
            gl_ctx.Disable(gles11::FOG);
            gl_ctx.DisableClientState(gles11::COLOR_ARRAY);
            gl_ctx.Color4f(1.0, 1.0, 1.0, 1.0);
            for mode in [gles11::PROJECTION, gles11::MODELVIEW, gles11::TEXTURE] {
                gl_ctx.MatrixMode(mode);
                gl_ctx.LoadIdentity();
            }
            let mut texture = 0;
            gl_ctx.GenTextures(1, &mut texture);
            gl_ctx.BindTexture(gles11::TEXTURE_2D, texture);
            gl_ctx.TexImage2D(
                gles11::TEXTURE_2D,
                0,
                gles11::RGBA as _,
                width as _,
                height as _,
                0,
                gles11::RGBA,
                gles11::UNSIGNED_BYTE,
                image.pixels().as_ptr() as *const _,
            );
            gl_ctx.TexParameteri(
                gles11::TEXTURE_2D,
                gles11::TEXTURE_MIN_FILTER,
                gles11::LINEAR as _,
            );
            gl_ctx.TexParameteri(
                gles11::TEXTURE_2D,
                gles11::TEXTURE_MAG_FILTER,
                gles11::LINEAR as _,
            );
            gl_ctx.BindFramebufferOES(gles11::FRAMEBUFFER_OES, 0);
            present_frame(gl_ctx.as_mut(), viewport, Matrix::y_flip(), None);
            gl_ctx.DeleteTextures(1, &texture);
        }
    }

    pub fn swap_window(&mut self) {
        if self.video_frame.is_some() {
            self.draw_video_frame();
        }
        self.window.gl_swap_window();
    }

    /// Consider the emulated device to be rotated to a particular orientation.
    ///
    /// On a PC or laptop, this will make the window be rotated so the app
    /// content appears upright. On a mobile device, this might do something
    /// else, because the user can physically rotate the screen.
    pub fn rotate_device(&mut self, new_orientation: DeviceOrientation) {
        assert!(self.on_main_stack);
        if new_orientation == self.device_orientation {
            return;
        }

        if !self.fullscreen && !Self::rotatable_fullscreen() {
            let (width, height) = if Self::rotatable_fullscreen() {
                set_sdl2_orientation(new_orientation);
                rotate_fullscreen_size(new_orientation, self.window.size())
            } else {
                self.display_resolution.unwrap_or_else(|| {
                    size_for_orientation(self.device_family, new_orientation, self.scale_hack)
                })
            };

            // macOS quirk: when resizing the window, the new framebuffer's size
            // is apparently max(new_size, old_size) in each dimension, but the
            // viewport is positioned wrong on the y axis for some reason, so we
            // need to apply an offset.
            // Recreating the OpenGL context was an alternative workaround, but
            // that apparently stops other OpenGL contexts drawing to the
            // framebuffer!
            #[cfg(target_os = "macos")]
            {
                let (_old_width, old_height) = self.window.size();
                self.max_height = self.max_height.max(old_height).max(height);
                self.viewport_y_offset = self.max_height - height;
            }

            self.window.set_size(width, height).unwrap();
        }

        if Self::rotatable_fullscreen() {
            set_sdl2_orientation(new_orientation);
            // Hack: from reading SDL2's source code, it seems that SDL2 will
            // only re-do the orientation when changing whether a window is
            // "resizeable" (can be rotated). You can't set the resizeable state
            // on a fullscreen window, so it must be temporarily stop being
            // fulscreen.
            // Apparently, doing this does result in resizing the window.
            self.window
                .set_fullscreen(sdl2::video::FullscreenType::Off)
                .unwrap();
            unsafe {
                let window_raw = self.window.raw();
                sdl2_sys::SDL_SetWindowResizable(window_raw, sdl2_sys::SDL_bool::SDL_FALSE);
                sdl2_sys::SDL_SetWindowResizable(window_raw, sdl2_sys::SDL_bool::SDL_TRUE);
            }
            self.window
                .set_fullscreen(sdl2::video::FullscreenType::True)
                .unwrap();
        }

        self.device_orientation = new_orientation;

        if self.splash_image.is_some() {
            self.display_splash();
        }
    }

    pub fn device_family(&self) -> DeviceFamily {
        self.device_family
    }

    /// Returns the current device orientation
    pub fn current_rotation(&self) -> DeviceOrientation {
        self.device_orientation
    }

    /// Get the size in pixels of the window without rotation or scaling.
    ///
    /// The aspect ratio, scale and orientation reflect the guest app's view of
    /// the world.
    pub fn size_unrotated_unscaled(&self) -> (u32, u32) {
        size_for_orientation(
            self.device_family,
            DeviceOrientation::Portrait,
            NonZeroU32::new(1).unwrap(),
        )
    }

    pub(crate) fn sdl_window(&self) -> &sdl2::video::Window {
        &self.window
    }

    pub(crate) fn apply_display_settings(
        &mut self,
        settings: crate::display_settings::Settings,
    ) -> Result<(), String> {
        assert!(self.on_main_stack);
        let old_size = self.window.size();
        let old_fullscreen = self.fullscreen;
        let (width, height) = crate::display_settings::RESOLUTIONS[settings.resolution];
        let mode = if settings.fullscreen {
            sdl2::video::FullscreenType::Desktop
        } else {
            sdl2::video::FullscreenType::Off
        };
        let result = self.window.set_fullscreen(mode).and_then(|_| {
            if !settings.fullscreen {
                self.window
                    .set_size(width, height)
                    .map_err(|e| e.to_string())
            } else {
                Ok(())
            }
        });
        if let Err(error) = result {
            let _ = self.window.set_fullscreen(if old_fullscreen {
                sdl2::video::FullscreenType::Desktop
            } else {
                sdl2::video::FullscreenType::Off
            });
            if !old_fullscreen {
                let _ = self.window.set_size(old_size.0, old_size.1);
            }
            return Err(error);
        }
        #[cfg(target_os = "macos")]
        {
            // SDL's window/fullscreen resize updates the drawable. The old
            // orientation-resize workaround must not retain the fullscreen
            // height, or the windowed frame is drawn outside its drawable.
            self.max_height = self.window.size().1;
            self.viewport_y_offset = 0;
        }
        self.fullscreen = settings.fullscreen;
        self.display_resolution = Some((width, height));
        self.scale_hack = NonZeroU32::new(settings.scale).unwrap();
        Ok(())
    }

    /// Get the region of the on-screen window (x, y, width, height) used to
    /// display the app content.
    ///
    /// The aspect ratio of this region always reflects the guest app's view of
    /// the world, but the scale and orientation might not.
    pub fn viewport(&self) -> (u32, u32, u32, u32) {
        let (app_width, app_height) =
            size_for_orientation(self.device_family, self.device_orientation, self.scale_hack);
        if let Some((width, height)) = self.display_resolution {
            let (screen_width, screen_height) = self.window.drawable_size();
            let output = if self.fullscreen {
                let scale = (screen_width as f64 / width as f64)
                    .min(screen_height as f64 / height as f64)
                    .min(1.0);
                (
                    (width as f64 * scale).round() as u32,
                    (height as f64 * scale).round() as u32,
                )
            } else {
                (screen_width, screen_height)
            };
            let (x, y, w, h) = crate::display_settings::fit(output, (app_width, app_height));
            return (
                x + (screen_width - output.0) / 2,
                y + (screen_height - output.1) / 2,
                w,
                h,
            );
        }
        if !self.fullscreen && !Self::rotatable_fullscreen() {
            return (0, 0, app_width, app_height);
        }

        let (screen_width, screen_height) = self.window.drawable_size();

        let app_aspect = app_width as f32 / app_height as f32;
        let screen_aspect = screen_width as f32 / screen_height as f32;
        let (scaled_width, scaled_height) = if app_aspect < screen_aspect {
            (
                (screen_height as f32 * app_aspect).round() as u32,
                screen_height,
            )
        } else {
            (
                screen_width,
                (screen_width as f32 / app_aspect).round() as u32,
            )
        };
        let x = (screen_width - scaled_width) / 2;
        let y = (screen_height - scaled_height) / 2;
        (x, y, scaled_width, scaled_height)
    }

    /// Special offset to add to y co-ordinates, only when drawing to screen.
    pub fn viewport_y_offset(&self) -> u32 {
        #[cfg(target_os = "macos")]
        return self.viewport_y_offset;
        #[cfg(not(target_os = "macos"))]
        return 0;
    }

    /// Transformation matrix for transforming between the window's co-ordinate
    /// space and the app's original co-ordinate space when rotation is in use
    /// (see [Self::rotate_device]). This returns a matrix appropriate for
    /// rotating texture co-ordinates to display the image in the window; when
    /// rotating input co-ordinates, invert the matrix.
    pub fn rotation_matrix(&self) -> Matrix<2> {
        orientation_rotation(self.device_orientation)
    }

    /// A UIKit view can keep showing the launch image while the app starts.
    /// Match its decoded pixels so presentation adjustments cannot affect
    /// unrelated full-screen images in the game.
    pub fn is_launch_image(&self, image: &Image) -> bool {
        self.splash_image.as_ref().is_some_and(|launch| {
            launch.dimensions() == image.dimensions() && launch.pixels() == image.pixels()
        })
    }

    pub fn is_screen_saver_enabled(&self) -> bool {
        self.video_ctx.is_screen_saver_enabled()
    }
    pub fn set_screen_saver_enabled(&mut self, enabled: bool) {
        assert!(self.on_main_stack);
        match enabled {
            true => self.video_ctx.enable_screen_saver(),
            false => self.video_ctx.disable_screen_saver(),
        }
    }

    pub fn start_text_input(&self) {
        assert!(self.on_main_stack);
        unsafe {
            sdl2_sys::SDL_StartTextInput();
        }
    }
    pub fn stop_text_input(&self) {
        assert!(self.on_main_stack);
        unsafe {
            sdl2_sys::SDL_StopTextInput();
        }
    }

    pub fn on_main_stack(&self) -> bool {
        self.on_main_stack
    }
}

pub fn open_url(env: &mut Environment, url: &str) -> Result<(), String> {
    env.on_parent_stack_in_coroutine(|_, _| sdl2::url::open_url(url).map_err(|e| e.to_string()))
}

/// Show an SDL messagebox for an error (typically after a panic).
///
/// The window argument allows for passing in the parent window for the
/// messagebox, which is not required but should be done if possible.
pub fn show_error_messagebox(window: Option<&Window>, error_message: &str) {
    assert!(window.is_none_or(|win| win.on_main_stack));
    use sdl2::messagebox;
    let mbox = [
        messagebox::ButtonData {
            flags: messagebox::MessageBoxButtonFlag::NOTHING,
            button_id: 0,
            text: "Open touchHLE directory",
        },
        messagebox::ButtonData {
            flags: messagebox::MessageBoxButtonFlag::NOTHING,
            button_id: 1,
            text: "Close",
        },
    ];

    let Ok(clicked_button) = messagebox::show_message_box(
        messagebox::MessageBoxFlag::ERROR,
        &mbox,
        "touchHLE crashed!",
        &format!("touchHLE crashed with the following error: {error_message}"),
        window.map(|win| &win.window),
        None,
    ) else {
        panic!("Failed to show message box!");
    };

    match clicked_button {
        messagebox::ClickedButton::CloseButton => {}
        messagebox::ClickedButton::CustomButton(button) => {
            match button.button_id {
                // Open data directory (contains log file on android)
                0 => match crate::paths::url_for_opening_user_data_dir() {
                    Ok(url) => {
                        if let Err(e) = sdl2::url::open_url(&url).map_err(|e| e.to_string()) {
                            echo!("Couldn't open file manager at {:?}: {}", url, e);
                        } else {
                            echo!("Opened file manager at {:?}, exiting.", url);
                        }
                    }
                    Err(e) => echo!("Couldn't open file manager: {}", e),
                },
                // Close
                1 => {}
                _ => unreachable!(),
            }
        }
    }
}

/// Get current battery state from SDL2.
///
/// Returns:
/// - pct: i32 - percentage of battery remaining.
/// - status: [BatteryState] - the current status of the battery
///   (unplugged, charging, full, etc.)
pub fn get_battery_status() -> (i32, BatteryState) {
    if env::consts::OS == "android" {
        log_once!(
            "Warning: get_battery_status on Android, returning fully charged to avoid SDL crash"
        );
        // Android_JNI_GetPowerInfo is crashing with `JNI DETECTED ERROR IN
        // APPLICATION: JNI ERROR (app bug): jobject is an invalid JNI
        // transition frame reference: 0x7b1bc0c7a0 (use of invalid jobject)`
        // TODO: See if updating SDL fixes that
        return (100, BatteryState::Full);
    }
    let mut pct = 0;
    // Unfortunately, Rust-SDL2 does not expose this function yet.
    // iPhoneOS does not measure the battery in seconds remaining,
    // so we discard this argument.
    let status = unsafe { sdl2_sys::SDL_GetPowerInfo(null_mut(), &mut pct) };
    (
        pct,
        match status {
            SDL_PowerState::SDL_POWERSTATE_UNKNOWN => BatteryState::Unknown,
            SDL_PowerState::SDL_POWERSTATE_ON_BATTERY => BatteryState::OnBattery,
            SDL_PowerState::SDL_POWERSTATE_NO_BATTERY => BatteryState::NoBattery,
            SDL_PowerState::SDL_POWERSTATE_CHARGING => BatteryState::Charging,
            SDL_PowerState::SDL_POWERSTATE_CHARGED => BatteryState::Full,
        },
    )
}

pub fn get_preferred_language_codes(env: &mut Environment) -> Vec<String> {
    env.on_parent_stack_in_coroutine(|_, _| {
        sdl2::locale::get_preferred_locales()
            .map(|loc| loc.lang)
            .collect()
    })
}

pub fn get_preferred_country_codes(env: &mut Environment) -> Vec<String> {
    env.on_parent_stack_in_coroutine(|_, _| {
        sdl2::locale::get_preferred_locales()
            .filter_map(|loc| loc.country)
            .collect()
    })
}

/// This entry point exists only in the explicitly opted-in test binary.
#[cfg(feature = "rebellion-test-tools")]
fn show_rebellion_test_menu(window: &sdl2::video::Window) -> Option<u8> {
    use sdl2::messagebox::*;
    let labels = [
        "Close",
        "Coins: 9999 / undo",
        "Posters: collect / undo",
        "Activate next achievement",
    ];
    let buttons: Vec<_> = labels
        .iter()
        .enumerate()
        .map(|(i, text)| ButtonData {
            flags: MessageBoxButtonFlag::NOTHING,
            button_id: i as i32,
            text,
        })
        .collect();
    match show_message_box(MessageBoxFlag::INFORMATION, &buttons,
        "8-Bit Rebellion test tools",
        "Use in the playable world with movement keys released.\nCoins and posters toggle back to their original values.\nAchievements: one per click, IDs 22000-22015; see the run log.\nRestart the test session to undo all test changes.", Some(window), None) {
        Ok(ClickedButton::CustomButton(button)) if button.button_id > 0 => Some(button.button_id as u8),
        _ => None,
    }
}

#[cfg(test)]
#[test]
fn keyboard_targets_follow_live_guest_control_scheme() {
    use crate::options::Button;
    let mut options = Options::default();
    options.button_to_touch.insert(Button::DPadLeft, (40.0, 290.0));
    options.button_to_touch.insert(Button::DPadRight, (135.0, 290.0));
    options.button_to_touch.insert(Button::A, (455.0, 290.0));
    // These are the predicates in AvatarComponent's Touch handler. Right
    // must reach the right strip; Space must reach the central attack zone.
    for touch_mode in [false, true, false, true] {
        options.keyboard_touch_controls = touch_mode;
        let right = keyboard_button_touch(&options, Button::DPadRight).unwrap();
        let attack = keyboard_button_touch(&options, Button::A).unwrap();
        if touch_mode {
            assert!(right.0 > 480.0 - 110.0 && right.1 > 160.0);
            assert!(attack.0 >= 110.0 && attack.0 <= 480.0 - 110.0 && attack.1 > 160.0);
            assert!(keyboard_button_touch(&options, Button::DPadLeft).unwrap().0 < 110.0);
        } else {
            assert_eq!(right, (135.0, 290.0));
            assert_eq!(attack, (455.0, 290.0));
        }
    }
}

#[cfg(test)]
#[test]
fn modal_key_release_requires_a_fresh_press() {
    let mut keys = HashSet::new();
    assert!(update_game_key(&mut keys, GameKey::D, true, false));
    keys.clear(); // Opening F9 releases input, even if SDL consumes KeyUp.
    assert!(!update_game_key(&mut keys, GameKey::D, true, true));
    assert_eq!(keyboard_horizontal_direction(&keys), None);
    assert!(!update_game_key(&mut keys, GameKey::D, false, false));
    assert!(update_game_key(&mut keys, GameKey::A, true, false));
    assert_eq!(
        keyboard_horizontal_direction(&keys),
        Some(crate::options::Button::DPadLeft)
    );
    assert!(update_game_key(&mut keys, GameKey::A, false, false));
    assert_eq!(keyboard_horizontal_direction(&keys), None);
}

#[cfg(test)]
#[test]
fn modal_discards_pending_controls_but_preserves_quit() {
    use crate::options::Button;
    for key in [
        GameKey::D,
        GameKey::Right,
        GameKey::A,
        GameKey::Left,
        GameKey::Space,
    ] {
        let mut keys = HashSet::from([key]);
        let mut direction = keyboard_horizontal_direction(&keys);
        let mut queue = VecDeque::from([
            Event::TouchesDown(HashMap::from([(
                FingerId::KeyboardDirection,
                (135.0, 290.0),
            )])),
            Event::TouchesDown(HashMap::from([(FingerId::Mouse, (440.0, 290.0))])),
            Event::TouchesUp(HashMap::from([(
                FingerId::KeyboardAttack,
                (455.0, 290.0),
            )])),
            Event::ToggleGameMenu,
            Event::ActivateDoor,
            Event::ShowDisplaySettings,
            Event::Quit,
        ]);
        discard_queued_game_input(&mut keys, &mut direction, &mut queue);
        assert!(keys.is_empty());
        assert_eq!(direction, None);
        assert_eq!(queue.len(), 1);
        assert!(matches!(queue.pop_front(), Some(Event::Quit)));
        // Neither a repeat nor a late release may restart walking/attacking.
        assert!(!update_game_key(&mut keys, key, true, true));
        assert!(!update_game_key(&mut keys, key, false, false));
        assert!(update_game_key(&mut keys, GameKey::Right, true, false));
        assert_eq!(
            keyboard_horizontal_direction(&keys),
            Some(Button::DPadRight)
        );
        assert!(update_game_key(&mut keys, GameKey::Right, false, false));
        assert_eq!(keyboard_horizontal_direction(&keys), None);
    }
}

#[cfg(test)]
#[test]
fn dock_icon_preserves_guest_artwork_inside_transparent_margins() {
    let mut source = vec![0; 57 * 57 * 4];
    for (i, byte) in source.iter_mut().enumerate() {
        *byte = (i % 251) as u8;
    }
    let icon = macos_dock_icon(Image::from_pixel_vec(source.clone(), (57, 57)));
    assert_eq!(icon.dimensions(), (71, 71));
    for y in 0..71 {
        for x in 0..71 {
            let actual = &icon.pixels()[(y * 71 + x) * 4..][..4];
            if (7..64).contains(&x) && (7..64).contains(&y) {
                assert_eq!(actual, &source[((y - 7) * 57 + x - 7) * 4..][..4]);
            } else {
                assert_eq!(actual, &[0, 0, 0, 0]);
            }
        }
    }
}

#[cfg(test)]
#[test]
fn dock_icon_centers_non_square_artwork_without_stretching() {
    let icon = macos_dock_icon(Image::from_pixel_vec(vec![255; 16 * 8 * 4], (16, 8)));
    assert_eq!(icon.dimensions(), (20, 20));
    for y in 0..20 {
        for x in 0..20 {
            let expected = if (2..18).contains(&x) && (6..14).contains(&y) { 255 } else { 0 };
            assert_eq!(&icon.pixels()[(y * 20 + x) * 4..][..4], &[expected; 4]);
        }
    }
}
