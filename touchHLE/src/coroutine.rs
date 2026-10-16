/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! Stackful coroutines, used to run each guest thread on its own host stack.
//!
//! Natively these are corosensei's. corosensei switches stacks with assembly,
//! which WebAssembly can't do, so emscripten builds use emscripten's
//! Asyncify-based fibers behind the subset of corosensei's API that touchHLE
//! uses.

#[cfg(not(target_os = "emscripten"))]
pub use corosensei::{Coroutine, CoroutineResult, Yielder};

#[cfg(target_os = "emscripten")]
pub use fiber::{Coroutine, CoroutineResult, Yielder};

#[cfg(target_os = "emscripten")]
mod fiber {
    use std::ffi::c_void;

    /// `asyncify_data_t` from `emscripten/fiber.h`.
    #[repr(C)]
    struct AsyncifyData {
        stack_ptr: *mut c_void,
        stack_limit: *mut c_void,
        rewind_id: i32,
    }

    /// `emscripten_fiber_t` from `emscripten/fiber.h`.
    #[repr(C)]
    struct Fiber {
        stack_base: *mut c_void,
        stack_limit: *mut c_void,
        stack_ptr: *mut c_void,
        entry: Option<unsafe extern "C" fn(*mut c_void)>,
        user_data: *mut c_void,
        asyncify_data: AsyncifyData,
    }

    extern "C" {
        fn emscripten_fiber_init(
            fiber: *mut Fiber,
            entry_func: unsafe extern "C" fn(*mut c_void),
            entry_func_arg: *mut c_void,
            c_stack: *mut c_void,
            c_stack_size: usize,
            asyncify_stack: *mut c_void,
            asyncify_stack_size: usize,
        );
        fn emscripten_fiber_init_from_current_context(
            fiber: *mut Fiber,
            asyncify_stack: *mut c_void,
            asyncify_stack_size: usize,
        );
        fn emscripten_fiber_swap(old_fiber: *mut Fiber, new_fiber: *mut Fiber);
    }

    /// Size of each coroutine's C (shadow) stack. WebAssembly has no guard
    /// pages, so an overflow would silently corrupt the heap: be generous.
    const C_STACK_SIZE: usize = 8 << 20;
    /// Size of each buffer Asyncify saves an unwound call stack into.
    const ASYNCIFY_STACK_SIZE: usize = 1 << 20;

    /// Memory for a stack. LLVM assumes the WebAssembly shadow stack is
    /// 16-byte aligned (it may compute addresses of aligned stack slots with
    /// a bitwise OR), so a misaligned stack silently corrupts values such as
    /// i128s. Plain `Vec<u8>` allocations don't guarantee that alignment.
    struct StackBuffer(*mut u8, usize);

    impl StackBuffer {
        const ALIGN: usize = 16;
        fn new(size: usize) -> Self {
            let layout = std::alloc::Layout::from_size_align(size, Self::ALIGN).unwrap();
            let ptr = unsafe { std::alloc::alloc_zeroed(layout) };
            assert!(!ptr.is_null());
            StackBuffer(ptr, size)
        }
        fn as_mut_ptr(&mut self) -> *mut u8 {
            self.0
        }
    }

    impl Drop for StackBuffer {
        fn drop(&mut self) {
            let layout = std::alloc::Layout::from_size_align(self.1, Self::ALIGN).unwrap();
            unsafe { std::alloc::dealloc(self.0, layout) }
        }
    }

    pub enum CoroutineResult<Y, R> {
        Yield(Y),
        Return(R),
    }

    /// Lets a coroutine suspend itself. Only valid inside the coroutine.
    pub struct Yielder<Input, Yield> {
        fiber: *mut Fiber,
        caller: *mut Fiber,
        input: *mut Option<Input>,
        yielded: *mut Option<Yield>,
    }

    impl<Input, Yield> Yielder<Input, Yield> {
        /// Return `val` to the code that resumed this coroutine, and wait to
        /// be resumed again.
        pub fn suspend(&self, val: Yield) -> Input {
            unsafe {
                *self.yielded = Some(val);
                emscripten_fiber_swap(self.fiber, self.caller);
                (*self.input).take().unwrap()
            }
        }

        /// corosensei runs `f` on the resumer's stack. There is only one
        /// thread on the web and no platform code cares which stack it runs
        /// on, so this just calls `f`.
        pub fn on_parent_stack<F: FnOnce() -> R, R>(&self, f: F) -> R {
            f()
        }
    }

    type Body<Input, Yield, Return> = Box<dyn FnOnce(&Yielder<Input, Yield>, Input) -> Return>;

    /// State shared between a coroutine and its resumer. It is boxed so its
    /// address stays fixed while either side holds pointers into it.
    struct Shared<Input, Yield, Return> {
        fiber: Fiber,
        caller: Fiber,
        input: Option<Input>,
        yielded: Option<Yield>,
        returned: Option<Return>,
        body: Option<Body<Input, Yield, Return>>,
        yielder: Option<Yielder<Input, Yield>>,
        c_stack: StackBuffer,
        asyncify_stack: StackBuffer,
        caller_asyncify_stack: StackBuffer,
    }

    pub struct Coroutine<Input, Yield, Return> {
        shared: Box<Shared<Input, Yield, Return>>,
        done: bool,
    }

    unsafe extern "C" fn entry<Input, Yield, Return>(arg: *mut c_void) {
        let shared = arg.cast::<Shared<Input, Yield, Return>>();
        unsafe {
            let body = (*shared).body.take().unwrap();
            let input = (*shared).input.take().unwrap();
            let yielder: *const Yielder<Input, Yield> = (*shared).yielder.as_ref().unwrap();
            let ret = body(&*yielder, input);
            (*shared).returned = Some(ret);
            // A fiber's entry function must never return.
            emscripten_fiber_swap(&mut (*shared).fiber, &mut (*shared).caller);
        }
        unreachable!();
    }

    impl<Input, Yield, Return> Coroutine<Input, Yield, Return> {
        pub fn new<F>(f: F) -> Self
        where
            F: FnOnce(&Yielder<Input, Yield>, Input) -> Return + 'static,
        {
            let zeroed_fiber = || Fiber {
                stack_base: std::ptr::null_mut(),
                stack_limit: std::ptr::null_mut(),
                stack_ptr: std::ptr::null_mut(),
                entry: None,
                user_data: std::ptr::null_mut(),
                asyncify_data: AsyncifyData {
                    stack_ptr: std::ptr::null_mut(),
                    stack_limit: std::ptr::null_mut(),
                    rewind_id: 0,
                },
            };
            let mut shared = Box::new(Shared {
                fiber: zeroed_fiber(),
                caller: zeroed_fiber(),
                input: None,
                yielded: None,
                returned: None,
                body: Some(Box::new(f)),
                yielder: None,
                c_stack: StackBuffer::new(C_STACK_SIZE),
                asyncify_stack: StackBuffer::new(ASYNCIFY_STACK_SIZE),
                caller_asyncify_stack: StackBuffer::new(ASYNCIFY_STACK_SIZE),
            });
            let ptr: *mut Shared<Input, Yield, Return> = &mut *shared;
            unsafe {
                (*ptr).yielder = Some(Yielder {
                    fiber: &mut (*ptr).fiber,
                    caller: &mut (*ptr).caller,
                    input: &mut (*ptr).input,
                    yielded: &mut (*ptr).yielded,
                });
                emscripten_fiber_init(
                    &mut (*ptr).fiber,
                    entry::<Input, Yield, Return>,
                    ptr.cast(),
                    (*ptr).c_stack.as_mut_ptr().cast(),
                    C_STACK_SIZE,
                    (*ptr).asyncify_stack.as_mut_ptr().cast(),
                    ASYNCIFY_STACK_SIZE,
                );
            }
            Coroutine {
                shared,
                done: false,
            }
        }

        /// Run the coroutine until it suspends or returns.
        pub fn resume(&mut self, val: Input) -> CoroutineResult<Yield, Return> {
            assert!(!self.done, "Resumed a coroutine that already returned");
            let shared: *mut Shared<Input, Yield, Return> = &mut *self.shared;
            unsafe {
                (*shared).input = Some(val);
                // The resumer can differ between calls (e.g. another
                // coroutine), so capture the current context every time.
                emscripten_fiber_init_from_current_context(
                    &mut (*shared).caller,
                    (*shared).caller_asyncify_stack.as_mut_ptr().cast(),
                    ASYNCIFY_STACK_SIZE,
                );
                emscripten_fiber_swap(&mut (*shared).caller, &mut (*shared).fiber);
                if let Some(ret) = (*shared).returned.take() {
                    self.done = true;
                    CoroutineResult::Return(ret)
                } else {
                    CoroutineResult::Yield((*shared).yielded.take().unwrap())
                }
            }
        }
    }
    // ponytail: dropping a suspended coroutine frees its stacks without
    // running destructors for values on them (corosensei unwinds instead).
    // Only matters for threads killed mid-run; add forced unwinding if leaks
    // show up.
}
