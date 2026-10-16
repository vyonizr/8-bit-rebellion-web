/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
// Allow the crate to have a non-snake-case name (touchHLE).
// This also allows items in the crate to have non-snake-case names.
#![allow(non_snake_case)]
// On emscripten, std's main wrapper catches panics with a WebAssembly
// exception handler, which Asyncify (needed to yield to the browser) can't
// unwind through. Export a plain C main instead.
#![cfg_attr(target_os = "emscripten", no_main)]

#[cfg(not(target_os = "emscripten"))]
fn main() -> Result<(), String> {
    touchHLE::main(std::env::args())
}

#[cfg(target_os = "emscripten")]
#[no_mangle]
pub extern "C" fn main(argc: std::ffi::c_int, argv: *const *const std::ffi::c_char) -> std::ffi::c_int {
    let args = (0..argc as usize).map(|i| unsafe {
        std::ffi::CStr::from_ptr(*argv.add(i))
            .to_string_lossy()
            .into_owned()
    });
    match touchHLE::main(args) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("touchHLE errored: {e}");
            1
        }
    }
}
