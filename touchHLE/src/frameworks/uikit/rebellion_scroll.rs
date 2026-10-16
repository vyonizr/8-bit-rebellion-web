/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! Mouse-wheel support for the game's OpenGL Scrolltable lists.
use crate::frameworks::core_graphics::{CGPoint, CGRect};
use crate::mem::Ptr;
use crate::objc::id;
use crate::{msg, Environment};

#[derive(Default)]
pub(super) struct State {
    drawing: Vec<(id, CGRect)>,
    visible: Vec<(id, CGRect)>,
}

fn supported(env: &Environment) -> bool {
    env.bundle.bundle_identifier() == "com.alife.linkinpark"
        && env.bundle.bundle_version() == "1.4.5"
}

/// Called only for Scrolltable's guest drawWithOffsetX:OffsetY: method.
/// Record draw order so hidden lists and lists behind other lists are skipped.
pub(crate) fn record_draw(env: &mut Environment, table: id) {
    if !supported(env) {
        return;
    }
    let mut rect: CGRect = env.mem.read((table.cast::<u8>() + 20).cast());
    rect.origin.x += f32::from_bits(env.cpu.regs()[2]);
    rect.origin.y += f32::from_bits(env.cpu.regs()[3]);
    let state = &mut env.framework_state.uikit.rebellion_scroll;
    state.drawing.push((table, rect));
}

pub(crate) fn present(env: &mut Environment) {
    if !supported(env) {
        return;
    }
    let state = &mut env.framework_state.uikit.rebellion_scroll;
    std::mem::swap(&mut state.visible, &mut state.drawing);
    state.drawing.clear();
}

fn offset_after_wheel(current: f32, minimum: f32, delta: f32) -> Option<f32> {
    if !current.is_finite() || !minimum.is_finite() || !delta.is_finite() || minimum >= 0.0 {
        return None;
    }
    Some((current + delta).clamp(minimum, 0.0))
}

pub(super) fn scroll(env: &mut Environment, cursor: CGPoint, delta: f32) {
    if !supported(env) {
        return;
    }
    let visible = env.framework_state.uikit.rebellion_scroll.visible.clone();
    for (table, rect) in visible.into_iter().rev() {
        if cursor.x < rect.origin.x
            || cursor.y < rect.origin.y
            || cursor.x >= rect.origin.x + rect.size.width
            || cursor.y >= rect.origin.y + rect.size.height
        {
            continue;
        }
        if env.objc.get_host_object(table).is_none()
            || env
                .objc
                .get_class_name(crate::objc::ObjC::read_isa(table, &env.mem))
                != "Scrolltable"
        {
            continue;
        }
        // IPA 1.4.5 metadata: m_pluggin at 12. The plugin's cycle and
        // moveToBottom methods establish offset=40, lower bound=48,
        // velocity=52, scripted-motion flag=9, scroll-enabled=80.
        let plugin: u32 = env.mem.read((table.cast::<u8>() + 12).cast());
        if plugin == 0
            || env
                .mem
                .get_bytes_fallible(Ptr::from_bits(plugin), 81)
                .is_none()
        {
            return;
        }
        let enabled: u8 = env.mem.read(Ptr::<u8, false>::from_bits(plugin + 80));
        if enabled == 0 {
            return;
        }
        let current: f32 = msg![env; table getOffsetY];
        let minimum: f32 = env.mem.read(Ptr::<f32, false>::from_bits(plugin + 48));
        if let Some(next) = offset_after_wheel(current, minimum, delta) {
            env.mem.write(Ptr::from_bits(plugin + 52), 0.0f32);
            env.mem.write(Ptr::from_bits(plugin + 9), 0u8);
            let _: () = msg![env; table setOffsetY:next];
            log!(
                "8-Bit Rebellion wheel scroll: {:?} {} -> {} (minimum {})",
                table,
                current,
                next,
                minimum
            );
        }
        return; // Only the last drawn list under the cursor receives input.
    }
}

#[cfg(test)]
mod tests {
    use super::offset_after_wheel;
    #[test]
    fn wheel_scroll_preserves_fractional_motion_and_clamps_both_edges() {
        assert_eq!(offset_after_wheel(-40.0, -100.0, -0.5), Some(-40.5));
        assert_eq!(offset_after_wheel(-40.0, -100.0, -200.0), Some(-100.0));
        assert_eq!(offset_after_wheel(-40.0, -100.0, 200.0), Some(0.0));
        assert_eq!(offset_after_wheel(0.0, 0.0, -40.0), None);
        assert_eq!(offset_after_wheel(f32::NAN, -100.0, -40.0), None);
    }
}
