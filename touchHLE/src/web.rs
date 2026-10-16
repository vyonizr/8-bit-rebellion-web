/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! Browser integration for the emscripten build.
//!
//! The browser only renders frames and delivers input while WebAssembly code
//! isn't running, so the emulator's main loop must regularly give control back.
//! These functions do that using Asyncify, which unwinds the stack to the
//! browser event loop and later rewinds it. They must only be called on the
//! main stack (outside guest-thread coroutines), which is where
//! [crate::Environment::run] schedules threads.
//!
//! The JavaScript side is `web/touchhle_lib.js`.

use std::cell::Cell;
use std::time::{Duration, Instant};

extern "C" {
    fn emscripten_sleep(ms: u32);
    /// Return to the event loop and resume as soon as possible, without the
    /// 4ms minimum delay of nested `setTimeout` calls.
    fn touchhle_yield();
}

/// How long the emulator may run before letting the browser handle input and
/// render.
const MAX_BUSY_TIME: Duration = Duration::from_millis(12);

thread_local! {
    static LAST_YIELD: Cell<Option<Instant>> = const { Cell::new(None) };
}

fn mark_yielded() {
    LAST_YIELD.with(|c| c.set(Some(Instant::now())));
}

/// Sleep without blocking the browser.
pub fn sleep(duration: Duration) {
    let ms = duration.as_millis().min(u32::MAX as u128) as u32;
    unsafe {
        if ms == 0 {
            touchhle_yield();
        } else {
            emscripten_sleep(ms);
        }
    }
    mark_yielded();
}

/// Yield to the browser if the emulator has been busy for too long.
pub fn maybe_yield() {
    let due = LAST_YIELD.with(|c| c.get().is_none_or(|t| t.elapsed() >= MAX_BUSY_TIME));
    if due {
        unsafe { touchhle_yield() };
        mark_yielded();
    }
}
