/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! The UIKit framework.
//!
//! For the time being the focus of this project is on running games, which are
//! likely to use UIKit in very simple and limited ways, so this implementation
//! will probably take a lot of shortcuts.

use crate::{msg, Environment};
use std::time::Instant;

pub(crate) mod rebellion_progress;
pub(crate) mod rebellion_scroll;
pub(crate) mod rebellion_render;
#[cfg(feature = "rebellion-test-tools")]
mod rebellion_test_tools;
pub mod ui_accelerometer;
pub mod ui_activity_indicator_view;
pub mod ui_application;
pub mod ui_bar_button_item;
pub mod ui_color;
pub mod ui_device;
pub mod ui_event;
pub mod ui_font;
pub mod ui_geometry;
pub mod ui_graphics;
pub mod ui_image;
pub mod ui_image_picker_controller;
pub mod ui_local_notification;
pub mod ui_navigation_item;
pub mod ui_nib;
pub mod ui_pasteboard;
pub mod ui_responder;
pub mod ui_screen;
pub mod ui_touch;
pub mod ui_view;
pub mod ui_view_controller;

pub const DYLIB: crate::dyld::HostDylib = crate::dyld::HostDylib {
    path: "/System/Library/Frameworks/UIKit.framework/UIKit",
    aliases: &[],
    class_exports: &[
        ui_accelerometer::CLASSES,
        ui_activity_indicator_view::CLASSES,
        ui_application::CLASSES,
        ui_bar_button_item::CLASSES,
        ui_color::CLASSES,
        ui_device::CLASSES,
        ui_event::CLASSES,
        ui_font::CLASSES,
        ui_image::CLASSES,
        ui_image_picker_controller::CLASSES,
        ui_local_notification::CLASSES,
        ui_nib::CLASSES,
        ui_navigation_item::CLASSES,
        ui_pasteboard::CLASSES,
        ui_responder::CLASSES,
        ui_screen::CLASSES,
        ui_touch::CLASSES,
        ui_view::CLASSES,
        ui_view::ui_alert_view::CLASSES,
        ui_view::ui_control::CLASSES,
        ui_view::ui_control::ui_button::CLASSES,
        ui_view::ui_control::ui_segmented_control::CLASSES,
        ui_view::ui_control::ui_slider::CLASSES,
        ui_view::ui_control::ui_text_field::CLASSES,
        ui_view::ui_control::ui_switch::CLASSES,
        ui_view::ui_image_view::CLASSES,
        ui_view::ui_label::CLASSES,
        ui_view::ui_navigation_bar::CLASSES,
        ui_view::ui_page_control::CLASSES,
        ui_view::ui_picker_view::CLASSES,
        ui_view::ui_scroll_view::CLASSES,
        ui_view::ui_scroll_view::ui_text_view::CLASSES,
        ui_view::ui_web_view::CLASSES,
        ui_view::ui_window::CLASSES,
        ui_view_controller::CLASSES,
        ui_view_controller::ui_navigation_controller::CLASSES,
        ui_view_controller::ui_table_view_controller::CLASSES,
    ],
    constant_exports: &[
        ui_application::CONSTANTS,
        ui_device::CONSTANTS,
        ui_view::ui_control::ui_text_field::CONSTANTS,
        ui_view::ui_window::CONSTANTS,
    ],
    function_exports: &[
        ui_application::FUNCTIONS,
        ui_geometry::FUNCTIONS,
        ui_graphics::FUNCTIONS,
    ],
};

#[derive(Default)]
pub struct State {
    rebellion_posters: Option<rebellion_progress::PosterProgress>,
    rebellion_scroll: rebellion_scroll::State,
    rebellion_render: rebellion_render::State,
    #[cfg(feature = "rebellion-test-tools")]
    rebellion_test_tools: rebellion_test_tools::State,
    ui_accelerometer: ui_accelerometer::State,
    ui_application: ui_application::State,
    ui_color: ui_color::State,
    ui_device: ui_device::State,
    ui_font: ui_font::State,
    ui_graphics: ui_graphics::State,
    ui_image: ui_image::State,
    ui_screen: ui_screen::State,
    ui_touch: ui_touch::State,
    pub ui_view: ui_view::State,
    ui_responder: ui_responder::State,
}

/// For use by `NSRunLoop`: handles any events that have queued up.
///
/// Returns the next time this function must be called, if any, e.g. the next
/// time an accelerometer input is due.
pub fn handle_events(env: &mut Environment) -> Option<Instant> {
    use crate::window::Event;
    use crate::window::TextInputEvent;

    // NSRunLoop will never call this function in headless mode.
    sync_8bit_rebellion_keyboard_scheme(env);
    while let Some(event) = env.window_mut().pop_event() {
        match event {
            Event::Quit => {
                echo!("User requested quit, exiting.");
                ui_application::exit(env);
            }
            Event::TouchesDown(..) | Event::TouchesMove(..) | Event::TouchesUp(..) => {
                ui_touch::handle_event(env, event)
            }
            Event::ShowDisplaySettings => {
                // The native modal can consume releases and generate mouse or
                // arrow-key events of its own. Stop accepted guest input first.
                ui_touch::end_all_touches(env);
                env.on_parent_stack_in_coroutine(|window, _| window.discard_modal_input());
                rebellion_render::reset(env);
                // Host modal menus suspend video consumption. Stop movie audio
                // too, then resume from the last presented frame on dismissal.
                crate::frameworks::media_player::set_muted(env, true);
                let action = env.on_parent_stack_in_coroutine(|window, options| {
                    crate::display_settings::show(
                        window.sdl_window(),
                        crate::display_settings::Settings::from_options(options),
                        options.sound_muted,
                    )
                });
                if let Some(action) = action {
                    let current = crate::display_settings::Settings::from_options(&env.options);
                    if action.display == current || crate::display_settings::apply(env, action.display) {
                        crate::sound_settings::set_muted(env, action.muted);
                    }
                }
                let muted = env.openal_manager.is_muted();
                crate::frameworks::media_player::set_muted(env, muted);
                // Do this after Apply too: resizing/focus changes may pump SDL.
                env.on_parent_stack_in_coroutine(|window, _| window.discard_modal_input());
            }
            Event::ActivateDoor => activate_8bit_rebellion_door(env),
            Event::ToggleGameMenu => toggle_8bit_rebellion_menu(env),
            Event::Scroll { cursor, delta } => rebellion_scroll::scroll(
                env,
                crate::frameworks::core_graphics::CGPoint {
                    x: cursor.0,
                    y: cursor.1,
                },
                delta,
            ),
            #[cfg(feature = "rebellion-test-tools")]
            Event::TestCheat(action) => rebellion_test_tools::apply(env, action),
            Event::AppWillResignActive => {
                // Getting this event means touchHLE is becoming inactive, e.g.
                // due to switching apps. The obvious way to handle this would
                // be to just send `applicationWillResignActive:` to the
                // UIApplicationDelegate. However:
                // - touchHLE's event loop can't handle an inactive app well
                //   right now. For example, audio isn't paused.
                // - touchHLE's event loop can't handle the subsequent
                //   termination of an app right now: it doesn't manage to send
                //   the `applicationWillTerminate:` message in time. This can
                //   mean loss of data!
                // Therefore, for the moment we will simulate the early iOS
                // behavior where switching app usually resulted in termination.
                // We can usually handle this in time, so there won't be data
                // loss, nor problems with background resource usage or audio.
                // TODO: Handle this better.
                log!("Handling app-will-resign-active event: exiting.");
                ui_application::exit(env);
            }
            Event::AppWillTerminate => {
                log!("Handling app-will-terminate event.");
                ui_application::exit(env);
            }
            Event::EnterDebugger => {
                if env.is_debugging_enabled() {
                    log!("Handling EnterDebugger event: entering debugger.");
                    env.enter_debugger(/* reason: */ None);
                } else {
                    log!("Ignoring EnterDebugger event: no debugger connected.");
                }
            }
            Event::TextInput(text_event) => {
                log!("DEBUG text event {:?}", text_event);
                let responder = env.framework_state.uikit.ui_responder.first_responder;
                let class = msg![env; responder class];
                let ui_text_field_class = env.objc.get_known_class("UITextField", &mut env.mem);
                if !responder.is_null() && env.objc.class_is_subclass_of(class, ui_text_field_class)
                {
                    match text_event {
                        TextInputEvent::Text(text) => {
                            ui_view::ui_control::ui_text_field::handle_text(env, responder, text)
                        }
                        TextInputEvent::Backspace => {
                            ui_view::ui_control::ui_text_field::handle_backspace(env, responder)
                        }
                        TextInputEvent::Return => {
                            ui_view::ui_control::ui_text_field::handle_return(env, responder)
                        }
                    }
                }
            }
        }
    }

    sync_8bit_rebellion_keyboard_scheme(env);
    sync_8bit_rebellion_walk_direction(env);
    rebellion_progress::sync_posters(env);
    let accelerometer = ui_accelerometer::handle_accelerometer(env);
    let render = rebellion_render::tick(env);
    accelerometer.into_iter().chain(render).min()
}

/// Ask the game's live setting instead of assuming its saved/default scheme.
fn sync_8bit_rebellion_keyboard_scheme(env: &mut Environment) {
    if !env.options.keyboard_game_controls
        || env.bundle.bundle_identifier() != "com.alife.linkinpark"
        || env.bundle.bundle_version() != "1.4.5"
        || !env.objc.all_classes().any(|(name, _)| name.as_str() == "AvatarComponent")
    {
        return;
    }
    let mode: i32 = crate::msg_class![env; AvatarComponent getControlMode];
    let touch = mode == 1;
    if env.options.keyboard_touch_controls != touch {
        log!("Keyboard control scheme: {}", if touch { "Touch" } else { "D-Pad" });
        env.options.keyboard_touch_controls = touch;
    }
}

/// Follow DoorEntryComponent's own completed-button path, without relying on
/// the on-screen icon's moving position. The field offsets are from the
/// Objective-C metadata in the user-supplied 1.4.5 executable.
fn activate_8bit_rebellion_door(env: &mut Environment) {
    use crate::frameworks::core_graphics::CGPoint;
    use crate::objc::id;

    if env.bundle.bundle_identifier() != "com.alife.linkinpark"
        || env.bundle.bundle_version() != "1.4.5"
    {
        return;
    }
    let Some(class) = env
        .objc
        .all_classes()
        .find(|(name, _)| name.as_str() == "DoorEntryComponent")
        .map(|(_, &class)| class)
    else {
        return;
    };
    let candidates = env.objc.instances_of_class(class, &env.mem);
    let active: Vec<_> = candidates
        .into_iter()
        .filter_map(|component| {
            let base = component.cast::<u8>();
            let state: i32 = env.mem.read((base + 48).cast());
            let door_index: i32 = env.mem.read((base + 64).cast());
            let door_pos: CGPoint = env.mem.read((base + 68).cast());
            let delegate: id = env.mem.read((base + 80).cast());
            ((state == 2 || state == 3) && door_index >= 0 && !delegate.is_null())
                .then_some((delegate, door_index, door_pos))
        })
        .collect();
    if let [(delegate, door_index, door_pos)] = active.as_slice() {
        let (delegate, door_index, door_pos) = (*delegate, *door_index, *door_pos);
        log!("Keyboard activating visible door {}", door_index);
        let _: () = msg![env; delegate enterDoor:door_index Position:door_pos];
    }
}

fn toggle_8bit_rebellion_menu(env: &mut Environment) {
    if env.bundle.bundle_identifier() != "com.alife.linkinpark"
        || env.bundle.bundle_version() != "1.4.5"
    {
        return;
    }
    let Some(class) = env
        .objc
        .all_classes()
        .find(|(name, _)| name.as_str() == "SceneUIMenuBar")
        .map(|(_, &class)| class)
    else {
        return;
    };
    let menus = env.objc.instances_of_class(class, &env.mem);
    if let [menu] = menus.as_slice() {
        let menu = *menu;
        log!("Keyboard toggling side menu");
        let _: () = msg![env; menu nextMode];
    }
}

/// The guest's moveLeft/RightShouldRun: only accept states 0, 1, 2 and 20.
/// During an attack the component sets isWalkHold, but direction cannot change.
/// Its next cycle then chooses movement from the old facing direction. Ending
/// the attack touch also clears isWalkHold despite the direction still being held.
fn needs_walk_direction_retry(
    state: i32,
    facing_right: bool,
    held_right: bool,
    holding: bool,
) -> bool {
    matches!(state, 0 | 1 | 2 | 20) && (!holding || facing_right != held_right)
}

fn sync_8bit_rebellion_walk_direction(env: &mut Environment) {
    use crate::objc::{id, nil, ObjC};
    use crate::options::Button;

    if env.bundle.bundle_identifier() != "com.alife.linkinpark"
        || env.bundle.bundle_version() != "1.4.5"
    {
        return;
    }
    let held_right = match env.window().held_keyboard_direction() {
        Some(Button::DPadRight) => true,
        Some(Button::DPadLeft) => false,
        _ => return,
    };
    let Some(view) = ui_touch::keyboard_direction_view(env) else {
        return;
    };
    if env.objc.get_class_name(ObjC::read_isa(view, &env.mem)) != "GCEAGLView" {
        return;
    }
    let Some(class) = env
        .objc
        .all_classes()
        .find(|(name, _)| name.as_str() == "AvatarComponent")
        .map(|(_, &class)| class)
    else {
        return;
    };
    let components = env.objc.instances_of_class(class, &env.mem);
    let [component] = components.as_slice() else {
        return;
    };
    // Offsets from the Objective-C metadata in the local 1.4.5 executable.
    let base = component.cast::<u8>();
    let holding: i8 = env.mem.read((base + 88).cast());
    let player: id = env.mem.read((base + 60).cast());
    if player == nil {
        return;
    }
    let state: i32 = msg![env; player m_state];
    let facing_right: bool = msg![env; player isFaceRight];
    if !needs_walk_direction_retry(state, facing_right, held_right, holding != 0) {
        return;
    }
    let running: i8 = env.mem.read((base + 89).cast());
    if held_right {
        let _: () = msg![env; player moveRightShouldRun:running];
    } else {
        let _: () = msg![env; player moveLeftShouldRun:running];
    }
    let now_right: bool = msg![env; player isFaceRight];
    let next_state: i32 = msg![env; player m_state];
    if now_right == held_right && matches!(next_state, 1 | 2) {
        // touchesEnded: for the attack also clears isWalkHold. Restore it only
        // after the game's own movement method actually accepted the input.
        env.mem.write((base + 88).cast::<i8>(), 1);

        log!(
            "8-Bit Rebellion restored held movement after attack: state={}, right={}",
            state,
            now_right
        );
    }
}

#[cfg(test)]
mod rebellion_walk_tests {
    use super::needs_walk_direction_retry;

    #[test]
    fn held_reverse_direction_waits_for_movement_state_in_both_directions() {
        for facing_right in [false, true] {
            for state in -1..=27 {
                let movable = matches!(state, 0 | 1 | 2 | 20);
                assert_eq!(
                    needs_walk_direction_retry(state, facing_right, !facing_right, true),
                    movable
                );
                assert!(!needs_walk_direction_retry(
                    state,
                    facing_right,
                    facing_right,
                    true
                ));
                assert_eq!(
                    needs_walk_direction_retry(state, facing_right, facing_right, false),
                    movable
                );
            }
        }
    }
}

/// MapComponent's assign delegate can be destroyed by selectLocation: while
/// touch dispatch still traverses the map buttons. Keep it alive for this
/// event, and clear a dangling delegate before a second click is delivered.
fn retain_8bit_rebellion_map_delegate(env: &mut Environment) -> crate::objc::id {
    use crate::objc::{id, nil, retain};
    if env.bundle.bundle_identifier() != "com.alife.linkinpark"
        || env.bundle.bundle_version() != "1.4.5"
    {
        return nil;
    }
    let Some(class) = env
        .objc
        .all_classes()
        .find(|(name, _)| name.as_str() == "MapComponent")
        .map(|(_, &class)| class)
    else {
        return nil;
    };
    let maps = env.objc.instances_of_class(class, &env.mem);
    let [map] = maps.as_slice() else {
        return nil;
    };
    // MapComponent.delegate is an assign field at offset 132 in IPA 1.4.5.
    let field = (map.cast::<u8>() + 132).cast::<id>();
    let delegate: id = env.mem.read(field);
    if delegate == nil {
        return nil;
    }
    if env.objc.get_host_object(delegate).is_none() {
        env.mem.write(field, nil);
        log!("8-Bit Rebellion cleared expired map delegate before repeated touch");
        return nil;
    }
    retain(env, delegate);
    delegate
}
