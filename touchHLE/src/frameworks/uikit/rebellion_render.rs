/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! Optional presentation-only camera and player interpolation for the inspected 1.4.5.
//! The game's controller and all cycle methods retain their original 15 Hz.
use crate::mem::{ConstPtr, Ptr};
use crate::objc::{id, nil};
use crate::{msg, Environment};
use std::collections::HashMap;
use std::time::{Duration, Instant};

const STEP: Duration = Duration::from_nanos(1_000_000_000 / 15);
const FRAME: Duration = Duration::from_nanos(1_000_000_000 / 60);
type Position = [f32; 2];
// Keep a fixed phase rather than accumulating callback/scheduler overhead.
fn next_deadline(due: Instant, now: Instant) -> Instant {
    let missed = now.saturating_duration_since(due).as_nanos() / FRAME.as_nanos();
    due + FRAME * (missed.min(u32::MAX as u128 - 1) as u32 + 1)
}

#[derive(Clone, Copy)]
struct Motion {
    previous: Position,
    current: Position,
    time: Instant,
}
impl Motion {
    fn new(position: Position, time: Instant) -> Self {
        Self {
            previous: position,
            current: position,
            time,
        }
    }
    fn update(&mut self, position: Position, time: Instant) {
        // A pause/teleport is a discontinuity, not a movement to interpolate.
        let continuous = time.duration_since(self.time) < STEP * 3
            && position
                .iter()
                .zip(self.current)
                .all(|(a, b)| a.is_finite() && (*a - b).abs() < 96.0);
        self.previous = if continuous { self.current } else { position };
        self.current = position;
        self.time = time;
    }
    fn offset(&self, time: Instant) -> Position {
        let alpha =
            (time.saturating_duration_since(self.time).as_secs_f32() / STEP.as_secs_f32()).min(1.0);
        std::array::from_fn(|i| (self.previous[i] - self.current[i]) * (1.0 - alpha))
    }
}
#[derive(Clone, Copy)]
struct PlayerMotion {
    avatar: id,
    scene: id,
    motion: Motion,
    sampled_frame: Instant,
}
#[derive(Clone, Copy, Default)]
struct Scope {
    offset: Position,
    backgrounds: bool,
    world_clipping: bool,
    time: Option<Instant>,
}
#[derive(Default)]
pub(super) struct State {
    view: Option<id>,
    scene: Option<id>,
    camera: Option<Motion>,
    player: Option<PlayerMotion>,
    layers: HashMap<id, Motion>,
    scopes: HashMap<usize, Scope>,
    next_frame: Option<Instant>,
    last_draw: Option<Instant>,
    frame_scene: Option<id>,
    world_view: Option<id>,
    extra_draw: Option<usize>,
    intermediate_frames: u8,
}
#[derive(Default)]
pub(crate) struct Call {
    pub(crate) skip: bool,
    scope: Option<Scope>,
    sample_scene: Option<id>,
    original_draw: bool,
}
pub(crate) fn is_intermediate_frame(env: &Environment) -> bool {
    env.framework_state.uikit.rebellion_render.extra_draw == Some(env.current_thread)
}
pub(crate) fn reset(env: &mut Environment) {
    env.framework_state.uikit.rebellion_render = State::default();
}

/// Observe original draws and interpolate only intermediate world frames.
/// Field offsets come from the supplied executable metadata.
pub(crate) fn before(env: &mut Environment, class: &str, selector: &str, receiver: id) -> Call {
    if !env.options.display_service {
        return Call::default();
    }
    let extra_draw = is_intermediate_frame(env);
    if skip_animation_step(extra_draw, class, selector) {
        return Call {
            skip: true,
            ..Call::default()
        };
    }
    let original_draw = class == "GCEAGLPanel" && selector == "adapter:drawRect:" && !extra_draw;
    if original_draw {
        let view = Ptr::from_bits(env.cpu.regs()[2]);
        let state = &mut env.framework_state.uikit.rebellion_render;
        let now = Instant::now();
        state.view = Some(view);
        state.last_draw = Some(now);
        state.frame_scene = None;
        state.world_view = None;
        // Never coalesce an original draw: its 15 Hz animation side effects
        // belong to the simulation. At most three extra frames fit each tick.
        state.intermediate_frames = 0;
        state.next_frame = Some(now + FRAME);
    }
    if !env.options.smooth_scrolling {
        return Call {
            original_draw,
            ..Call::default()
        };
    }
    if class == "Scene" && selector == "cycle" {
        return Call {
            sample_scene: Some(receiver),
            ..Call::default()
        };
    }
    if class == "AvatarComponent" && selector == "drawPlayerAvatar" && !extra_draw {
        sample_player(env, receiver);
    }
    let overlay_scene = match (class, selector) {
        ("DoorEntryComponent", "draw") => {
            let player: id = env.mem.read((receiver.cast::<u8>() + 40).cast());
            (player != nil).then(|| env.mem.read((player.cast::<u8>() + 184).cast()))
        }
        ("BulletinComponent", "draw") => Some(env.mem.read((receiver.cast::<u8>() + 52).cast())),
        _ => None,
    };
    let state = &mut env.framework_state.uikit.rebellion_render;
    let old = state
        .scopes
        .get(&env.current_thread)
        .copied()
        .unwrap_or_default();
    let mut scope = old;
    let mut change = false;
    match (class, selector) {
        ("GCEAGLPanel", "adapter:drawRect:") => {
            scope = Scope {
                time: Some(Instant::now()),
                ..Scope::default()
            };
            change = true;
        }
        ("Scene", "draw") if state.scene == Some(receiver) => {
            if !extra_draw {
                state.frame_scene = Some(receiver);
            }
            scope.backgrounds = true;
            change = true;
        }
        ("M2DMotionWelder", "draw") if old.backgrounds => {
            scope.offset = state.layers.get(&receiver).map_or([0.0; 2], |motion| {
                motion.offset(old.time.unwrap_or_else(Instant::now))
            });
            scope.backgrounds = false;
            change = true;
        }
        ("Avatar" | "PlayerAvatar", "beforeDraw" | "draw" | "afterDraw")
            if state.player.is_some_and(|player| {
                player.avatar == receiver && Some(player.scene) == state.scene
            }) =>
        {
            // Cover body, shadow and attached emotions without translating the
            // BulletManager drawn later by AvatarComponent.drawPlayerAvatar.
            // Replace, rather than add to, nested player/superclass scopes.
            scope.offset = state.player_offset(old.time.unwrap_or_else(Instant::now));
            change = true;
        }
        ("DoorEntryComponent" | "BulletinComponent", "draw") => {
            // These world overlays are separate from Scene.draw. Their cached
            // positions must follow the same camera phase as the background.
            scope.offset =
                state.overlay_offset(overlay_scene, old.time.unwrap_or_else(Instant::now));
            scope.world_clipping = class == "BulletinComponent"
                && overlay_scene == state.scene
                && state.frame_scene == state.scene
                && state.scene.is_some();
            change = true;
        }
        ("Scene", "drawSceneItemsWithLine:")
        | ("AvatarComponent", "drawPlayerAvatar")
        | ("AvatarComponent", "drawNPCWithLine:")
        | ("AvatarComponent", "drawOtherPlayerAvatar")
        | ("AvatarComponent", "beforeDrawAvatar")
        | ("AvatarComponent", "afterDrawAvatar")
        | ("AvatarComponent", "beforeDrawAllAvatar")
        | ("AvatarComponent", "afterDrawAllAvatar")
        | ("BulletManager", "draw")
        | ("EffectComponent", "draw") => {
            scope.offset = state.camera.map_or([0.0; 2], |motion| {
                motion.offset(old.time.unwrap_or_else(Instant::now))
            });
            change = true;
        }
        _ => (),
    }
    if change {
        state.scopes.insert(env.current_thread, scope);
    }
    Call {
        original_draw,
        scope: change.then_some(old),
        ..Call::default()
    }
}

pub(crate) fn after(env: &mut Environment, call: Call) {
    if call.original_draw {
        let state = &mut env.framework_state.uikit.rebellion_render;
        // The splash, title, loading screens and menus never draw a Scene.
        // Reject stale camera history even if they reuse the same EAGL view.
        state.world_view = state
            .frame_scene
            .filter(|scene| Some(*scene) == state.scene)
            .and(state.view);
    }
    if let Some(scene) = call.sample_scene {
        sample(env, scene);
    }
    if let Some(scope) = call.scope {
        env.framework_state
            .uikit
            .rebellion_render
            .scopes
            .insert(env.current_thread, scope);
    }
}
fn sample(env: &mut Environment, scene: id) {
    let now = Instant::now();
    let position: Position = [
        env.mem.read((scene.cast::<u8>() + 48).cast()),
        env.mem.read((scene.cast::<u8>() + 52).cast()),
    ];
    let vector: u32 = env.mem.read((scene.cast::<u8>() + 8).cast());
    let mut positions = Vec::new();
    if env
        .mem
        .get_bytes_fallible(ConstPtr::from_bits(vector), 8)
        .is_some()
    {
        let begin: u32 = env.mem.read(ConstPtr::from_bits(vector));
        let end: u32 = env.mem.read(ConstPtr::from_bits(vector + 4));
        if end >= begin
            && (end - begin) % 4 == 0
            && end - begin <= 64 * 4
            && env
                .mem
                .get_bytes_fallible(ConstPtr::from_bits(begin), end - begin)
                .is_some()
        {
            for address in (begin..end).step_by(4) {
                let layer: u32 = env.mem.read(ConstPtr::from_bits(address));
                if env
                    .mem
                    .get_bytes_fallible(ConstPtr::from_bits(layer), 4)
                    .is_none()
                {
                    continue;
                }
                let welder: id = env.mem.read(ConstPtr::from_bits(layer));
                if env
                    .mem
                    .get_bytes_fallible(welder.cast().cast_const(), 92)
                    .is_none()
                {
                    continue;
                }
                let xy: [i32; 2] = [
                    env.mem.read((welder.cast::<u8>() + 84).cast()),
                    env.mem.read((welder.cast::<u8>() + 88).cast()),
                ];
                positions.push((welder, xy.map(|value| value as f32)));
            }
        }
    }
    let state = &mut env.framework_state.uikit.rebellion_render;
    if state.scene != Some(scene) {
        state.scene = Some(scene);
        state.camera = None;
        state.player = None;
        state.layers.clear();
    }
    if let Some(camera) = &mut state.camera {
        camera.update(position, now);
    } else {
        state.camera = Some(Motion::new(position, now));
    }
    state
        .layers
        .retain(|id, _| positions.iter().any(|(layer, _)| layer == id));
    for (id, position) in positions {
        state
            .layers
            .entry(id)
            .and_modify(|motion| motion.update(position, now))
            .or_insert_with(|| Motion::new(position, now));
    }
}
/// Capture the same simulation snapshot as the camera, once per original draw.
/// Never change Avatar.m_position or the POD animation's stored position.
fn sample_player(env: &mut Environment, component: id) {
    let avatar: id = env.mem.read((component.cast::<u8>() + 60).cast());
    if avatar == nil
        || env
            .mem
            .get_bytes_fallible(avatar.cast().cast_const(), 188)
            .is_none()
    {
        env.framework_state.uikit.rebellion_render.player = None;
        return;
    }
    let scene: id = env.mem.read((avatar.cast::<u8>() + 184).cast());
    let position: Position = [
        env.mem.read((avatar.cast::<u8>() + 12).cast()),
        env.mem.read((avatar.cast::<u8>() + 16).cast()),
    ];
    let state = &mut env.framework_state.uikit.rebellion_render;
    let Some(camera) = state.camera else {
        return;
    };
    if state.scene != Some(scene) || state.frame_scene != Some(scene) {
        state.player = None;
        return;
    }
    let frame = state.last_draw.unwrap_or(camera.time);
    if let Some(player) = &mut state.player {
        if player.avatar == avatar && player.scene == scene {
            if player.sampled_frame != frame {
                player.motion.update(position, camera.time);
                player.sampled_frame = frame;
            }
            return;
        }
    }
    state.player = Some(PlayerMotion {
        avatar,
        scene,
        motion: Motion::new(position, camera.time),
        sampled_frame: frame,
    });
}
/// These inspected void methods advance sprite state; draw itself is pure
/// for these renderers. Some UI/effect draw paths call cycle internally.
fn skip_animation_step(extra_draw: bool, class: &str, selector: &str) -> bool {
    extra_draw
        && match class {
            "M2DMotionWelder" => matches!(selector, "cycle" | "cycleToPreviousFrame"),
            "M2DPODAnimation" => matches!(
                selector,
                "cycle" | "cycleWithDeltaTime:" | "nextFrame" | "previousFrame"
            ),
            "M2DOpenGLImage" | "M2DImage" => matches!(selector, "nextFrame" | "previousFrame"),
            _ => false,
        }
}
fn is_moving(motion: Motion, now: Instant) -> bool {
    motion.previous != motion.current && now.saturating_duration_since(motion.time) < STEP
}
impl State {
    fn overlay_offset(&self, scene: Option<id>, time: Instant) -> Position {
        if scene.is_none() || scene != self.scene || self.frame_scene != self.scene {
            return [0.0; 2];
        }
        self.camera.map_or([0.0; 2], |motion| motion.offset(time))
    }
    fn player_offset(&self, now: Instant) -> Position {
        let camera = self.camera.map_or([0.0; 2], |motion| motion.offset(now));
        let player = self
            .player
            .map_or([0.0; 2], |player| player.motion.offset(now));
        std::array::from_fn(|i| camera[i] + player[i])
    }
    fn scrolling(&self, now: Instant) -> bool {
        self.world_view.is_some()
            && self.world_view == self.view
            && (self.camera.is_some_and(|motion| is_moving(motion, now))
                || self.player.is_some_and(|player| {
                    Some(player.scene) == self.scene && is_moving(player.motion, now)
                }))
    }
    fn extra_frame_due(&self, now: Instant) -> Option<Instant> {
        if !self.scrolling(now) || self.intermediate_frames >= 3 {
            return None;
        }
        self.next_frame
    }
}

/// Apply only inside world draw scopes. HUD/menu drawing remains unshifted.
pub(crate) fn drawing_offset(env: &Environment) -> Position {
    if !env.options.smooth_scrolling {
        return [0.0; 2];
    }
    env.framework_state
        .uikit
        .rebellion_render
        .scopes
        .get(&env.current_thread)
        .map_or([0.0; 2], |scope| scope.offset)
}
/// World bulletin clipping moves with its geometry; ordinary HUD clipping does not.
pub(crate) fn world_clipping(env: &Environment) -> bool {
    env.options.smooth_scrolling
        && env
            .framework_state
            .uikit
            .rebellion_render
            .scopes
            .get(&env.current_thread)
            .is_some_and(|scope| scope.world_clipping)
}
/// Main-thread presentation requests. No guest cycle/timer is called here.
pub(super) fn tick(env: &mut Environment) -> Option<Instant> {
    if !env.options.smooth_scrolling
        || !env.options.display_service
        || env.window().is_video_active()
    {
        return None;
    }
    let now = Instant::now();
    let state = &mut env.framework_state.uikit.rebellion_render;
    let view = state.view?;
    if state
        .last_draw
        .is_none_or(|time| now.duration_since(time) > Duration::from_secs(1))
    {
        return None;
    }
    let due = state.extra_frame_due(now)?;
    if now < due {
        return Some(due);
    }
    let next = next_deadline(due, now);
    if env.objc.get_host_object(view).is_none()
        || env
            .objc
            .get_class_name(crate::objc::ObjC::read_isa(view, &env.mem))
            != "GCEAGLView"
    {
        reset(env);
        return None;
    }
    let window: id = msg![env; view window];
    let hidden: bool = msg![env; view isHidden];
    if window != nil && !hidden {
        // CAEAGLLayer drawing is managed by the game, not a UIKit bitmap
        // context. Use its existing drawRect delegate path directly.
        let bounds: crate::frameworks::core_graphics::CGRect = msg![env; view bounds];
        env.framework_state.uikit.rebellion_render.extra_draw = Some(env.current_thread);
        let _: () = msg![env; view drawRect:bounds];
        env.framework_state.uikit.rebellion_render.extra_draw = None;
    }
    let state = &mut env.framework_state.uikit.rebellion_render;
    state.intermediate_frames += 1;
    state.next_frame = Some(next);
    state.extra_frame_due(Instant::now())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn presentation_deadlines_do_not_drift_or_replay_missed_frames() {
        let start = Instant::now();
        let next = next_deadline(start, start + Duration::from_millis(2));
        assert_eq!(next, start + FRAME);
        assert_eq!(
            next_deadline(next, next + Duration::from_millis(2)),
            start + FRAME * 2
        );
        assert_eq!(
            next_deadline(start, start + FRAME * 6 + Duration::from_millis(2)),
            start + FRAME * 7
        );
    }
    #[test]
    fn four_presentation_frames_interpolate_one_original_tick() {
        let time = Instant::now();
        let mut motion = Motion::new([0.0, 0.0], time);
        motion.update([-8.0, 0.0], time + STEP);
        for (frame, expected) in [(0, 8.0), (1, 6.0), (2, 4.0), (3, 2.0), (4, 0.0)] {
            assert!((motion.offset(time + STEP + FRAME * frame)[0] - expected).abs() < 0.0001);
        }
        // Drawing samples must never advance the simulation or mutate history.
        assert_eq!(motion.current, [-8.0, 0.0]);
    }
    #[test]
    fn reversal_stop_pause_and_teleport_do_not_extrapolate() {
        let time = Instant::now();
        let mut motion = Motion::new([0.0, 0.0], time);
        motion.update([-8.0, 0.0], time + STEP);
        motion.update([-4.0, 0.0], time + STEP * 2);
        assert_eq!(motion.offset(time + STEP * 2), [-4.0, 0.0]);
        motion.update([-4.0, 0.0], time + STEP * 3);
        assert_eq!(motion.offset(time + STEP * 3), [0.0, 0.0]);
        motion.update([-12.0, 0.0], time + STEP * 10);
        assert_eq!(motion.offset(time + STEP * 10), [0.0, 0.0]);
        motion.update([-500.0, 0.0], time + STEP * 11);
        assert_eq!(motion.offset(time + STEP * 11), [0.0, 0.0]);
    }
    #[test]
    fn parallax_can_move_by_fractional_pixels_independently_of_foreground() {
        let time = Instant::now();
        let mut background = Motion::new([0.0, 0.0], time);
        background.update([-1.0, 0.0], time + STEP);
        assert!((background.offset(time + STEP + FRAME)[0] - 0.75).abs() < 0.0001);
        assert_eq!(Scope::default().offset, [0.0; 2]);
    }
    #[test]
    fn only_scrolling_world_frames_request_intermediate_redraws() {
        let now = Instant::now();
        let view = Ptr::from_bits(0x1000);
        let mut state = State {
            view: Some(view),
            next_frame: Some(now + FRAME),
            ..State::default()
        };
        // Startup/title/loading screens have no Scene even if Smooth is saved On.
        assert_eq!(state.extra_frame_due(now), None);
        let mut motion = Motion::new([0.0, 0.0], now);
        state.camera = Some(motion);
        state.world_view = Some(view);
        assert_eq!(state.extra_frame_due(now), None); // Idle location stays 15 Hz.
        motion.update([-8.0, 0.0], now + STEP);
        state.camera = Some(motion);
        for frame in 0..3 {
            state.intermediate_frames = frame;
            assert!(state
                .extra_frame_due(now + STEP + FRAME * frame as u32)
                .is_some());
        }
        state.intermediate_frames = 3;
        assert_eq!(state.extra_frame_due(now + STEP + FRAME * 3), None);
        state.intermediate_frames = 0;
        assert_eq!(state.extra_frame_due(now + STEP * 2), None); // Pause/stale cycle.
        state.world_view = None; // Non-world draw reused the same EAGL view.
        assert_eq!(state.extra_frame_due(now + STEP), None);
        state.world_view = Some(Ptr::from_bits(0x2000));
        assert_eq!(state.extra_frame_due(now + STEP), None);
    }
    #[test]
    fn player_and_camera_share_phase_without_screen_position_sawtooth() {
        let time = Instant::now();
        let avatar = Ptr::from_bits(0x1000);
        let scene = Ptr::from_bits(0x2000);
        let mut state = State::default();
        for direction in [1.0, -1.0] {
            let mut camera = Motion::new([-100.0, 0.0], time);
            camera.update([-100.0 - direction * 10.0, 0.0], time + STEP);
            let mut player = Motion::new([340.0, 230.0], time);
            player.update([340.0 + direction * 10.0, 230.0], time + STEP);
            state.camera = Some(camera);
            state.player = Some(PlayerMotion {
                avatar,
                scene,
                motion: player,
                sampled_frame: time + STEP,
            });
            for frame in 0..=4 {
                assert_eq!(state.player_offset(time + STEP + FRAME * frame), [0.0; 2]);
            }
            // At the camera boundary the sprite still translates smoothly.
            state.camera = Some(Motion::new([-100.0, 0.0], time + STEP));
            for frame in 0..=4 {
                let offset = state.player_offset(time + STEP + FRAME * frame);
                let screen_x = player.current[0] - 100.0 + offset[0];
                let expected = 240.0 + direction * 2.5 * frame as f32;
                assert!((screen_x - expected).abs() < 0.0001);
            }
            assert_eq!(player.current, [340.0 + direction * 10.0, 230.0]);
        }
    }
    #[test]
    fn player_movement_at_camera_boundary_redraws_only_live_world() {
        let time = Instant::now();
        let scene = Ptr::from_bits(0x2000);
        let view = Ptr::from_bits(0x3000);
        let mut motion = Motion::new([340.0, 230.0], time);
        motion.update([350.0, 230.0], time + STEP);
        let mut state = State {
            scene: Some(scene),
            view: Some(view),
            world_view: Some(view),
            camera: Some(Motion::new([-100.0, 0.0], time + STEP)),
            player: Some(PlayerMotion {
                avatar: Ptr::from_bits(0x1000),
                scene,
                motion,
                sampled_frame: time + STEP,
            }),
            next_frame: Some(time + STEP + FRAME),
            ..State::default()
        };
        assert!(state.extra_frame_due(time + STEP).is_some());
        assert_eq!(state.extra_frame_due(time + STEP * 2), None);
        state.world_view = None;
        assert_eq!(state.extra_frame_due(time + STEP), None);
        state.world_view = Some(view);
        state.player.as_mut().unwrap().scene = Ptr::from_bits(0x4000);
        assert_eq!(state.extra_frame_due(time + STEP), None);
        state.player = None;
        assert_eq!(state.extra_frame_due(time + STEP), None);
    }
    #[test]
    fn world_overlays_follow_camera_phase_without_shifting_hud_or_stale_scene() {
        let time = Instant::now();
        let scene = Ptr::from_bits(0x2000);
        let mut camera = Motion::new([-100.0, 0.0], time);
        camera.update([-110.0, 0.0], time + STEP);
        let mut state = State {
            camera: Some(camera),
            scene: Some(scene),
            frame_scene: Some(scene),
            ..State::default()
        };
        for frame in 0..=4 {
            let offset = state.overlay_offset(Some(scene), time + STEP + FRAME * frame);
            let door_x = 600.0 + camera.current[0] + offset[0];
            let board_x = 900.0 + camera.current[0] + offset[0];
            assert!((door_x - (500.0 - 2.5 * frame as f32)).abs() < 0.0001);
            assert_eq!(board_x - door_x, 300.0);
        }
        assert_eq!(state.overlay_offset(None, time + STEP), [0.0; 2]);
        assert_eq!(
            state.overlay_offset(Some(Ptr::from_bits(0x3000)), time + STEP),
            [0.0; 2]
        );
        state.frame_scene = None;
        assert_eq!(state.overlay_offset(Some(scene), time + STEP), [0.0; 2]);
        assert!(!Scope::default().world_clipping);
    }
    #[test]
    fn intermediate_drawing_does_not_step_sprite_animation() {
        for (class, selector) in [
            ("M2DMotionWelder", "cycle"),
            ("M2DMotionWelder", "cycleToPreviousFrame"),
            ("M2DPODAnimation", "cycleWithDeltaTime:"),
            ("M2DPODAnimation", "nextFrame"),
            ("M2DOpenGLImage", "nextFrame"),
            ("M2DImage", "previousFrame"),
        ] {
            assert!(skip_animation_step(true, class, selector));
            assert!(!skip_animation_step(false, class, selector));
        }
        assert!(!skip_animation_step(true, "M2DMotionWelder", "draw"));
        assert!(!skip_animation_step(true, "M2DPODAnimation", "draw"));
        assert!(!skip_animation_step(true, "PlayerAvatar", "cycle"));
    }
}
