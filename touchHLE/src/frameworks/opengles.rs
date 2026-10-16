/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! OpenGL ES and EAGL.
//!
//! This module is specific to OpenGL ES's role as a part of the iPhone OS API
//! surface. See [crate::gles] for other uses and a discussion of the broader
//! topic.

mod atlas_guard;
mod eagl;
mod gles_guest;

use touchHLE_gl_bindings::gles11::types::GLenum;

use crate::mem::ConstPtr;

pub const DYLIB: crate::dyld::HostDylib = crate::dyld::HostDylib {
    path: "/System/Library/Frameworks/OpenGLES.framework/OpenGLES",
    aliases: &[],
    class_exports: &[eagl::CLASSES],
    constant_exports: &[eagl::CONSTANTS],
    function_exports: &[gles_guest::FUNCTIONS],
};

#[derive(Default)]
pub struct State {
    /// Current EAGLContext for each thread
    current_ctxs: std::collections::HashMap<crate::ThreadId, Option<crate::objc::id>>,
    strings_cache: std::collections::HashMap<GLenum, ConstPtr<u8>>,
}
impl State {
    fn current_ctx_for_thread(&mut self, thread: crate::ThreadId) -> &mut Option<crate::objc::id> {
        self.current_ctxs.entry(thread).or_insert(None);
        self.current_ctxs.get_mut(&thread).unwrap()
    }
}

fn sync_context<'objc, 'win: 'objc>(
    state: &mut State,
    objc: &'objc mut crate::objc::ObjC,
    window: &'win mut crate::window::Window,
    current_thread: crate::ThreadId,
) -> Box<dyn crate::gles::GLES + 'objc> {
    let gles_ctx = get_thread_context(state, objc, current_thread);
    gles_ctx.make_current(window)
}

fn get_thread_context<'objc>(
    state: &mut State,
    objc: &'objc mut crate::objc::ObjC,
    current_thread: crate::ThreadId,
) -> &'objc mut dyn crate::gles::GLESContext {
    let current_ctx = state.current_ctx_for_thread(current_thread);
    let host_obj = objc.borrow_mut::<eagl::EAGLContextHostObject>(current_ctx.unwrap());
    let gles_ctx = host_obj.gles_ctx.as_deref_mut().unwrap();
    gles_ctx
}

/// Keep logical allocation dimensions for live render-scale changes. Names and
/// attachments are preserved; shared EAGL contexts share this registry.
fn record_renderbuffer(
    env: &mut crate::Environment,
    name: u32,
    format: GLenum,
    width: i32,
    height: i32,
) {
    if !env.options.display_service {
        return;
    }
    if let Some(context) = *env
        .framework_state
        .opengles
        .current_ctx_for_thread(env.current_thread)
    {
        env.objc
            .borrow_mut::<eagl::EAGLContextHostObject>(context)
            .renderbuffers
            .borrow_mut()
            .insert(name, (format, width, height));
    }
}

fn scaled_rectangle(rect: [i32; 4], old: u32, new: u32) -> [i32; 4] {
    rect.map(|value| (i64::from(value) * i64::from(new) / i64::from(old)) as i32)
}

pub(crate) fn resize_renderbuffers(
    env: &mut crate::Environment,
    old: u32,
    new: u32,
) -> Result<(), String> {
    if old == new {
        return Ok(());
    }
    use crate::gles::{gles11_raw as gl, GLES};
    struct Plan {
        context: crate::objc::id,
        buffers: Vec<(u32, (GLenum, i32, i32))>,
        viewport: [i32; 4],
        scissor: [i32; 4],
    }
    fn resize(gles: &mut dyn GLES, plan: &Plan, old: u32, scale: u32) -> Result<(), String> {
        let mut binding = 0;
        unsafe {
            gles.GetIntegerv(gl::RENDERBUFFER_BINDING_OES, &mut binding);
        }
        let mut result = Ok(());
        for &(name, (format, width, height)) in &plan.buffers {
            let (width, height) = (width * scale as i32, height * scale as i32);
            unsafe {
                gles.BindRenderbufferOES(gl::RENDERBUFFER_OES, name);
                gles.RenderbufferStorageOES(gl::RENDERBUFFER_OES, format, width, height);
                let (mut actual_width, mut actual_height) = (0, 0);
                gles.GetRenderbufferParameterivOES(
                    gl::RENDERBUFFER_OES,
                    gl::RENDERBUFFER_WIDTH_OES,
                    &mut actual_width,
                );
                gles.GetRenderbufferParameterivOES(
                    gl::RENDERBUFFER_OES,
                    gl::RENDERBUFFER_HEIGHT_OES,
                    &mut actual_height,
                );
                if (actual_width, actual_height) != (width, height) {
                    result = Err(format!(
                        "Could not resize renderbuffer {} to {} x {}",
                        name, width, height
                    ));
                    break;
                }
            }
        }
        unsafe {
            gles.BindRenderbufferOES(gl::RENDERBUFFER_OES, binding as u32);
            let [x, y, w, h] = scaled_rectangle(plan.viewport, old, scale);
            gles.Viewport(x, y, w, h);
            let [x, y, w, h] = scaled_rectangle(plan.scissor, old, scale);
            gles.Scissor(x, y, w, h);
        }
        result
    }
    let contexts = env
        .objc
        .all_classes()
        .find(|(name, _)| name.as_str() == "EAGLContext")
        .map(|(_, &class)| env.objc.instances_of_class(class, &env.mem))
        .unwrap_or_default();
    let mut plans = Vec::new();
    let mut groups = std::collections::HashSet::new();
    for context in contexts {
        let host = env.objc.borrow_mut::<eagl::EAGLContextHostObject>(context);
        let Some(ctx) = host.gles_ctx.as_mut() else {
            continue;
        };
        let mut gles = ctx.make_current(env.window.as_mut().unwrap());
        let mut plan = Plan {
            context,
            buffers: Vec::new(),
            viewport: [0; 4],
            scissor: [0; 4],
        };
        unsafe {
            gles.GetIntegerv(gl::VIEWPORT, plan.viewport.as_mut_ptr());
            gles.GetIntegerv(gl::SCISSOR_BOX, plan.scissor.as_mut_ptr());
        }
        if groups.insert(std::rc::Rc::as_ptr(&host.renderbuffers) as usize) {
            plan.buffers = host
                .renderbuffers
                .borrow()
                .iter()
                .map(|(&name, &allocation)| (name, allocation))
                .collect();
            let mut limit = 0;
            unsafe {
                gles.GetIntegerv(gl::MAX_RENDERBUFFER_SIZE_OES, &mut limit);
            }
            for &(_, (_, width, height)) in &plan.buffers {
                if width < 0
                    || height < 0
                    || i64::from(width) * i64::from(new) > i64::from(limit)
                    || i64::from(height) * i64::from(new) > i64::from(limit)
                {
                    return Err(format!(
                        "Render scale exceeds the GPU renderbuffer limit ({})",
                        limit
                    ));
                }
            }
        }
        plans.push(plan);
    }
    for (index, plan) in plans.iter().enumerate() {
        let result = {
            let host = env
                .objc
                .borrow_mut::<eagl::EAGLContextHostObject>(plan.context);
            let mut gles = host
                .gles_ctx
                .as_mut()
                .unwrap()
                .make_current(env.window.as_mut().unwrap());
            resize(gles.as_mut(), plan, old, new)
        };
        if let Err(error) = result {
            for rollback in &plans[..=index] {
                let host = env
                    .objc
                    .borrow_mut::<eagl::EAGLContextHostObject>(rollback.context);
                let mut gles = host
                    .gles_ctx
                    .as_mut()
                    .unwrap()
                    .make_current(env.window.as_mut().unwrap());
                if let Err(rollback_error) = resize(gles.as_mut(), rollback, old, old) {
                    log!("Renderbuffer rollback failed: {}", rollback_error);
                }
            }
            return Err(error);
        }
    }
    log!(
        "Resized guest renderbuffers live: {}x -> {}x ({} contexts)",
        old,
        new,
        plans.len()
    );
    Ok(())
}

#[cfg(test)]
#[test]
fn live_render_scale_restores_viewport_and_scissor_coordinates() {
    assert_eq!(scaled_rectangle([0, 0, 480, 320], 1, 4), [0, 0, 1920, 1280]);
    assert_eq!(
        scaled_rectangle([60, 110, 750, 490], 2, 3),
        [90, 165, 1125, 735]
    );
    assert_eq!(
        scaled_rectangle([90, 165, 1125, 735], 3, 2),
        [60, 110, 750, 490]
    );
}
