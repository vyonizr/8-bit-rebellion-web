/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `MPMoviePlayerController` etc.

use crate::dyld::{ConstantExports, HostConstant};
use crate::frameworks::foundation::{ns_string, ns_url, NSInteger, NSTimeInterval};
use crate::frameworks::uikit::ui_device::UIDeviceOrientation;
use crate::image::Image;
use crate::objc::{
    id, msg, msg_class, nil, objc_classes, release, retain, todo_objc_setter, ClassExports,
    HostObject, NSZonePtr,
};
use crate::Environment;
use std::collections::VecDeque;
#[cfg(not(target_os = "emscripten"))]
use std::io::{Read, Write};
#[cfg(not(target_os = "emscripten"))]
use std::process::{Child, Command, Stdio};
#[cfg(not(target_os = "emscripten"))]
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

// All three movies in this game's iPhone bundle use 480x320 at 30 fps.
const VIDEO_SIZE: (u32, u32) = (480, 320);
#[cfg(not(target_os = "emscripten"))]
const VIDEO_FPS: u64 = 30;
#[cfg(not(target_os = "emscripten"))]
const FRAME_BYTES: usize = VIDEO_SIZE.0 as usize * VIDEO_SIZE.1 as usize * 4;

#[cfg(not(target_os = "emscripten"))]
struct Playback {
    video: Child,
    audio: Option<MovieAudio>,
    audio_data: Arc<Vec<u8>>,
    frames_received: u64,
    next_frame: Instant,
    frames: mpsc::Receiver<Vec<u8>>,
}

#[cfg(not(target_os = "emscripten"))]
impl Playback {
    fn audio_position(&self) -> f64 {
        // The first presented frame is at time zero. Modal host menus do not
        // consume frames, so their wall-clock duration must not advance audio.
        self.frames_received.saturating_sub(1) as f64 / VIDEO_FPS as f64
    }

    fn stop(&mut self) {
        let _ = self.video.kill();
        let _ = self.video.wait();
        if let Some(audio) = self.audio.as_mut() {
            audio.stop();
        }
    }
}

#[cfg(not(target_os = "emscripten"))]
struct MovieAudio {
    decoder: Child,
    player: Child,
}
#[cfg(not(target_os = "emscripten"))]
impl MovieAudio {
    fn stop(&mut self) {
        let _ = self.player.kill();
        let _ = self.decoder.kill();
        let _ = self.player.wait();
        let _ = self.decoder.wait();
    }
}
#[cfg(not(target_os = "emscripten"))]
impl Drop for MovieAudio {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(not(target_os = "emscripten"))]
/// Decode forward through pipe input so unmuting can resume at the current video time.
fn start_audio(data: Arc<Vec<u8>>, position: f64) -> Option<MovieAudio> {
    let mut decoder = match Command::new("ffmpeg")
        .args([
            "-loglevel",
            "error",
            "-i",
            "pipe:0",
            "-ss",
            &format!("{position:.3}"),
            "-vn",
            "-ac",
            "2",
            "-ar",
            "44100",
            "-f",
            "wav",
            "pipe:1",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
    {
        Ok(process) => process,
        Err(error) => {
            log!("Could not start movie audio decoder: {}", error);
            return None;
        }
    };
    let input = decoder.stdout.take().unwrap();
    let player = match Command::new("ffplay")
        .args([
            "-loglevel",
            "error",
            "-autoexit",
            "-nodisp",
            "-vn",
            "-i",
            "pipe:0",
        ])
        .stdin(Stdio::from(input))
        .stdout(Stdio::null())
        .spawn()
    {
        Ok(process) => process,
        Err(error) => {
            let _ = decoder.kill();
            let _ = decoder.wait();
            log!("Movie audio unavailable: {}", error);
            return None;
        }
    };
    feed_process(&mut decoder, data);
    Some(MovieAudio { decoder, player })
}

#[cfg(not(target_os = "emscripten"))]
pub(crate) fn set_muted(env: &mut Environment, muted: bool) {
    if let Some(playback) = State::get(env).playback.as_mut() {
        if let Some(mut audio) = playback.audio.take() {
            audio.stop();
        }
        if !muted {
            playback.audio = start_audio(
                playback.audio_data.clone(),
                playback.audio_position(),
            );
        }
    }
}

#[derive(Default)]
pub struct State {
    active_player: Option<id>,
    playback: Option<Playback>,
    /// Various apps (e.g. Crash Bandicoot Nitro Kart 3D and Spore Origins)
    /// create or start a player and await some kind of notification, but can't
    /// handle it if that notification happens immediately. This queue lets us
    /// delay such notifications until the app next returns to the run loop,
    /// which seems to be late enough.
    pending_notifications: VecDeque<(&'static str, id, Instant)>,
}
impl State {
    fn get(env: &mut Environment) -> &mut Self {
        &mut env.framework_state.media_player.movie_player
    }
}

impl Drop for State {
    fn drop(&mut self) {
        if let Some(mut playback) = self.playback.take() {
            playback.stop();
        }
    }
}

#[cfg(not(target_os = "emscripten"))]
fn feed_process(child: &mut Child, data: Arc<Vec<u8>>) {
    let mut input = child.stdin.take().unwrap();
    std::thread::spawn(move || {
        let _ = input.write_all(&data);
    });
}

#[cfg(not(target_os = "emscripten"))]
fn start_video(env: &mut Environment, player: id) -> Option<Playback> {
    let url = env
        .objc
        .borrow::<MPMoviePlayerControllerHostObject>(player)
        .content_url;
    let path = ns_url::to_rust_path(env, url);
    let data = match env.fs.read(&path) {
        Ok(data) => data,
        Err(_) => {
            log!("Movie file could not be read: {:?}", path);
            return None;
        }
    };
    let data = Arc::new(data);
    let mut video = match Command::new("ffmpeg")
        .args([
            "-loglevel",
            "error",
            "-re",
            "-i",
            "pipe:0",
            "-an",
            "-vf",
            "scale=480:320,fps=30",
            "-pix_fmt",
            "rgba",
            "-f",
            "rawvideo",
            "pipe:1",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            log!("Could not start ffmpeg for {:?}: {}", path, error);
            return None;
        }
    };
    let mut output = video.stdout.take().unwrap();
    let (sender, frames) = mpsc::sync_channel(2);
    std::thread::spawn(move || loop {
        let mut frame = vec![0; FRAME_BYTES];
        if output.read_exact(&mut frame).is_err() || sender.send(frame).is_err() {
            break;
        }
    });
    feed_process(&mut video, data.clone());

    let audio = if env.openal_manager.is_muted() {
        None
    } else {
        start_audio(data.clone(), 0.0)
    };
    log!("Playing movie {:?} in the touchHLE window", path);
    Some(Playback {
        video,
        audio,
        audio_data: data,
        frames_received: 0,
        next_frame: Instant::now(),
        frames,
    })
}

/// In a browser, movies play in an HTML `<video>` element above the canvas
/// (see web/touchhle_lib.js), so the browser decodes them.
#[cfg(target_os = "emscripten")]
mod web_video {
    extern "C" {
        pub fn touchhle_video_play(data: *const u8, len: usize, muted: bool);
        /// Whether the movie ended or the user skipped it.
        pub fn touchhle_video_finished() -> bool;
        pub fn touchhle_video_stop();
        pub fn touchhle_video_set_muted(muted: bool);
    }
}

#[cfg(target_os = "emscripten")]
struct Playback;

#[cfg(target_os = "emscripten")]
impl Playback {
    fn stop(&mut self) {
        unsafe { web_video::touchhle_video_stop() }
    }
}

#[cfg(target_os = "emscripten")]
pub(crate) fn set_muted(_env: &mut Environment, muted: bool) {
    unsafe { web_video::touchhle_video_set_muted(muted) }
}

#[cfg(target_os = "emscripten")]
fn start_video(env: &mut Environment, player: id) -> Option<Playback> {
    let url = env
        .objc
        .borrow::<MPMoviePlayerControllerHostObject>(player)
        .content_url;
    let path = ns_url::to_rust_path(env, url);
    let Ok(data) = env.fs.read(&path) else {
        log!("Movie file could not be read: {:?}", path);
        return None;
    };
    let muted = env.openal_manager.is_muted();
    unsafe { web_video::touchhle_video_play(data.as_ptr(), data.len(), muted) };
    log!("Playing movie {:?} in the page", path);
    Some(Playback)
}

#[cfg(target_os = "emscripten")]
fn poll_playback(env: &mut Environment) -> (Option<Vec<u8>>, bool) {
    let playing = State::get(env).playback.is_some();
    (None, playing && unsafe { web_video::touchhle_video_finished() })
}

type MPMovieScalingMode = NSInteger;
type MPMovieControlStyle = NSInteger;

type MPMoviePlaybackState = NSInteger;
const MPMoviePlaybackStateStopped: MPMoviePlaybackState = 0;
const MPMoviePlaybackStatePlaying: MPMoviePlaybackState = 1;

// Values might not be correct, but as these are linked symbol constants, it
// shouldn't matter.
pub const MPMoviePlayerPlaybackDidFinishNotification: &str =
    "MPMoviePlayerPlaybackDidFinishNotification";
/// Apparently an undocumented, private API. Spore Origins uses it.
pub const MPMoviePlayerContentPreloadDidFinishNotification: &str =
    "MPMoviePlayerContentPreloadDidFinishNotification";
pub const MPMoviePlayerScalingModeDidChangeNotification: &str =
    "MPMoviePlayerScalingModeDidChangeNotification";
pub const MPMoviePlayerDidExitFullscreenNotification: &str =
    "MPMoviePlayerDidExitFullscreenNotification";
pub const MPMoviePlayerDidEnterFullscreenNotification: &str =
    "MPMoviePlayerDidEnterFullscreenNotification";
// TODO: More notifications?
const MPMoviePlayerPlaybackDidFinishReasonUserInfoKey: &str =
    "MPMoviePlayerPlaybackDidFinishReasonUserInfoKey";

/// `NSNotificationName` values and other constants.
pub const CONSTANTS: ConstantExports = &[
    (
        "_MPMoviePlayerPlaybackDidFinishNotification",
        HostConstant::NSString(MPMoviePlayerPlaybackDidFinishNotification),
    ),
    (
        "_MPMoviePlayerContentPreloadDidFinishNotification",
        HostConstant::NSString(MPMoviePlayerContentPreloadDidFinishNotification),
    ),
    (
        "_MPMoviePlayerScalingModeDidChangeNotification",
        HostConstant::NSString(MPMoviePlayerScalingModeDidChangeNotification),
    ),
    (
        "_MPMoviePlayerDidExitFullscreenNotification",
        HostConstant::NSString(MPMoviePlayerDidExitFullscreenNotification),
    ),
    (
        "_MPMoviePlayerDidEnterFullscreenNotification",
        HostConstant::NSString(MPMoviePlayerDidEnterFullscreenNotification),
    ),
    (
        "_MPMoviePlayerPlaybackDidFinishReasonUserInfoKey",
        HostConstant::NSString(MPMoviePlayerPlaybackDidFinishReasonUserInfoKey),
    ),
];

struct MPMoviePlayerControllerHostObject {
    // NSURL *
    content_url: id,
    fullscreen: bool,
}
impl HostObject for MPMoviePlayerControllerHostObject {}

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

@implementation MPMoviePlayerController: NSObject

// TODO: actual playback

+ (id)allocWithZone:(NSZonePtr)_zone {
    let host_object = Box::new(MPMoviePlayerControllerHostObject {
        content_url: nil,
        fullscreen: false,
    });
    env.objc.alloc_object(this, host_object, &mut env.mem)
}

- (id)initWithContentURL:(id)url { // NSURL*
    log!(
        "TODO: [(MPMoviePlayerController*){:?} initWithContentURL:{:?} ({:?})]",
        this,
        url,
        ns_url::to_rust_path(env, url),
    );

    retain(env, url);
    env.objc.borrow_mut::<MPMoviePlayerControllerHostObject>(this).content_url = url;

    // Act as if loading immediately completed (Spore Origins waits for this).
    State::get(env).pending_notifications.push_back(
        (MPMoviePlayerContentPreloadDidFinishNotification, this, Instant::now())
    );

    this
}

- (())dealloc {
    let url = env.objc.borrow::<MPMoviePlayerControllerHostObject>(this).content_url;
    release(env, url);

    env.objc.dealloc_object(this, &mut env.mem);
}

- (id)contentURL {
    env.objc.borrow::<MPMoviePlayerControllerHostObject>(this).content_url
}

- (id)backgroundColor {
    msg_class![env; UIColor blackColor] // TODO
}
- (())setBackgroundColor:(id)color { // UIColor*
    todo_objc_setter!(this, color);
}

- (())setScalingMode:(MPMovieScalingMode)mode {
    todo_objc_setter!(this, mode);
}
- (())setUseApplicationAudioSession:(bool)use_session {
    todo_objc_setter!(this, use_session);
}
- (())setControlStyle:(MPMovieControlStyle)style {
    todo_objc_setter!(this, style);
}
- (())setFullscreen:(bool)fullscreen {
    let state = env.objc.borrow_mut::<MPMoviePlayerControllerHostObject>(this);
    if state.fullscreen == fullscreen {
        return;
    }
    state.fullscreen = fullscreen;
    let notification = if fullscreen {
        MPMoviePlayerDidEnterFullscreenNotification
    } else {
        MPMoviePlayerDidExitFullscreenNotification
    };
    State::get(env)
        .pending_notifications
        .push_back((notification, this, Instant::now()));
}
- (())setFullscreen:(bool)fullscreen animated:(bool)_animated {
    let _: () = msg![env; this setFullscreen:fullscreen];
}
- (bool)isFullscreen {
    env.objc.borrow::<MPMoviePlayerControllerHostObject>(this).fullscreen
}
- (())setInitialPlaybackTime:(NSTimeInterval)initial_time {
    todo_objc_setter!(this, initial_time);
}

- (id)view {
    nil // TODO
}

- (MPMoviePlaybackState)playbackState {
    let state = State::get(env);
    if state.active_player == Some(this)
        && (state.playback.is_some()
            || state.pending_notifications.iter().any(|(name, object, _)| {
                *name == MPMoviePlayerPlaybackDidFinishNotification && *object == this
            }))
    {
        MPMoviePlaybackStatePlaying
    } else {
        MPMoviePlaybackStateStopped
    }
}

// Apparently an undocumented, private API, but Spore Origins uses it.
- (())setMovieControlMode:(NSInteger)_mode {
    // As this is undocumented and we don't have real video playback yet, let's
    // ignore it.
}

// Another undocumented one! But some apps may still use it :/
// https://stackoverflow.com/a/1390079/2241008
- (())setOrientation:(UIDeviceOrientation)_orientation animated:(bool)_animated {

}

// MPMediaPlayback implementation
- (())play {
    log!(
        "MPMoviePlayerController play: player {:?}, guest caller {:#x}",
        this,
        env.cpu.regs()[crate::cpu::Cpu::LR],
    );
    if let Some(old) = env.framework_state.media_player.movie_player.active_player {
        let _: () = msg![env; old stop];
    }
    assert!(env.framework_state.media_player.movie_player.active_player.is_none());
    // Movie player is retained by the runtime until it is stopped
    retain(env, this);
    env.framework_state.media_player.movie_player.active_player = Some(this);

    if let Some(playback) = start_video(env, this) {
        if let Some(window) = env.window.as_mut() {
            window.set_video_touch_active(true);
        }
        State::get(env).playback = Some(playback);
        return;
    }

    // Preserve the old behavior when playback is unavailable.
    // (various apps wait for this, such as BIA and Hero of Sparta).
    let notif = (MPMoviePlayerPlaybackDidFinishNotification, this, Instant::now().checked_add(Duration::from_millis(1000)).unwrap());
    for (name, obj, _) in &mut State::get(env).pending_notifications {
        // De-duplicate similar notifications. This can happen if app is calling
        // `play` twice on the same player object (case of NOVA2).
        if *name == MPMoviePlayerPlaybackDidFinishNotification && *obj == this {
            return;
        }
    }
    State::get(env).pending_notifications.push_back(notif);
}

- (())pause {
    log!("TODO: [(MPMoviePlayerController*){:?} pause]", this);
}

- (())stop {
    log!("TODO: [(MPMoviePlayerController*){:?} stop]", this);
    if let Some(mut playback) = State::get(env).playback.take() {
        playback.stop();
    }
    if let Some(window) = env.window.as_mut() {
        window.set_video_frame(None);
        window.set_video_touch_active(false);
    }
    State::get(env).pending_notifications.retain(|(name, object, _)| {
        !(*name == MPMoviePlayerPlaybackDidFinishNotification && *object == this)
    });
    if env.framework_state.media_player.movie_player.active_player.is_some() {
        // Some applications (like NOVA2) may send 2 `stop` messages for each
        // 1 `play` message for the player. In that case, we want to release
        // the active player only once.
        assert!(this == env.framework_state.media_player.movie_player.active_player.take().unwrap());
        release(env, this);
    }
}

@end

@implementation MPMoviePlayerViewController: UIViewController

- (id)initWithContentURL:(id)url {
    log!(
        "TODO: [(MPMoviePlayerViewController*){:?} initWithContentURL:{:?} ({:?})] -> nil",
        this,
        url,
        ns_url::to_rust_path(env, url),
    );
    release(env, this);
    nil // TODO
}

@end

};

/// For use by `NSRunLoop` via [super::handle_players]: check movie players'
/// status, send notifications if necessary.
pub(super) fn handle_players(env: &mut Environment) {
    let skipped = env
        .window
        .as_mut()
        .is_some_and(|window| window.take_video_skip_request());
    let (latest_frame, finished) = if skipped {
        (None, State::get(env).playback.is_some())
    } else {
        poll_playback(env)
    };
    if let Some(frame) = latest_frame {
        if let Some(window) = env.window.as_mut() {
            window.set_video_frame(Some(Image::from_pixel_vec(frame, VIDEO_SIZE)));
            window.swap_window();
        }
    }
    finish_players(env, skipped, finished);
}

#[cfg(not(target_os = "emscripten"))]
fn poll_playback(env: &mut Environment) -> (Option<Vec<u8>>, bool) {
    {
        let mut latest_frame = None;
        let mut frames_exhausted = false;
        let mut finished = false;
        if let Some(playback) = State::get(env).playback.as_mut() {
            let now = Instant::now();
            if now >= playback.next_frame {
                match playback.frames.try_recv() {
                    Ok(frame) => {
                        playback.frames_received += 1;
                        latest_frame = Some(frame);
                        // Do not fast-forward buffered video after a modal menu.
                        playback.next_frame = (playback.next_frame
                            + Duration::from_secs_f64(1.0 / VIDEO_FPS as f64)).max(now);
                    }
                    Err(mpsc::TryRecvError::Empty) => {},
                    Err(mpsc::TryRecvError::Disconnected) => {
                        frames_exhausted = true;
                    }
                }
            }
            let video_status = playback.video.try_wait();
            let video_done = video_status
                .as_ref()
                .map_or(true, |status| status.is_some());
            let video_failed = video_status
                .as_ref()
                .ok()
                .and_then(|status| status.as_ref())
                .is_some_and(|status| !status.success());
            let audio_done = playback.audio.as_mut().map_or(true, |audio| {
                audio
                    .player
                    .try_wait()
                    .map_or(true, |status| status.is_some())
            });
            finished = video_failed || (frames_exhausted && video_done && audio_done);
        }
        (latest_frame, finished)
    }
}

fn finish_players(env: &mut Environment, skipped: bool, finished: bool) {
    if finished {
        if skipped {
            log!("Movie playback skipped by touch");
        } else {
            log!("Movie playback finished");
        }
        if let Some(mut playback) = State::get(env).playback.take() {
            playback.stop();
        }
        if let Some(window) = env.window.as_mut() {
            window.set_video_frame(None);
            window.set_video_touch_active(false);
        }
        if let Some(player) = State::get(env).active_player {
            State::get(env).pending_notifications.push_back((
                MPMoviePlayerPlaybackDidFinishNotification,
                player,
                Instant::now(),
            ));
        }
    }
    let mut notifs_to_run = Vec::new();
    let pending_notifs = &mut State::get(env).pending_notifications;
    let mut i = 0;
    while i < pending_notifs.len() {
        let (name_str, object, time) = pending_notifs[i];
        if Instant::now() >= time {
            notifs_to_run.push((name_str, object));
            pending_notifs.swap_remove_back(i);
        } else {
            i += 1;
        }
    }
    for (name_str, object) in notifs_to_run {
        let name = ns_string::get_static_str(env, name_str);
        let center: id = msg_class![env; NSNotificationCenter defaultCenter];
        // TODO: should there be some user info attached?
        let _: () = msg![env; center postNotificationName:name object:object];
    }
}

#[cfg(all(test, not(target_os = "emscripten")))]
mod playback_position_tests {
    use super::*;

    #[test]
    fn queued_frames_do_not_advance_the_audio_resume_position() {
        // A harmless child supplies the fields required by Playback; no audio.
        let video = Command::new(if cfg!(windows) { "cmd" } else { "true" });
        let mut video = video;
        if cfg!(windows) { video.args(["/C", "exit", "0"]); }
        let (sender, frames) = mpsc::channel();
        let mut playback = Playback {
            video: video.spawn().unwrap(), audio: None,
            audio_data: Arc::new(Vec::new()), frames_received: 0, next_frame: Instant::now(), frames,
        };
        assert_eq!(playback.audio_position(), 0.0);
        // Five frames have been consumed, and more queue up while F9 blocks
        // the consumer. Seek to the displayed frame, not the decoder or clock.
        playback.frames_received = 5;
        for _ in 5..60 { sender.send(Vec::new()).unwrap(); }
        assert_eq!(playback.audio_position(), 4.0 / 30.0);
        while playback.frames.try_recv().is_ok() { playback.frames_received += 1; }
        assert_eq!(playback.audio_position(), 59.0 / 30.0);
        playback.stop();
    }
}
