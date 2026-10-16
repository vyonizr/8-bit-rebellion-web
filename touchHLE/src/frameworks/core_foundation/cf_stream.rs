/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `CFStream` socket stream creation.

use super::cf_allocator::CFAllocatorRef;
use super::cf_string::CFStringRef;
use super::CFTypeRef;
use crate::dyld::{export_c_func, FunctionExports};
use crate::mem::{MutPtr, Ptr};
use crate::Environment;

fn CFStreamCreatePairWithSocketToHost(
    env: &mut Environment,
    _allocator: CFAllocatorRef,
    _host: CFStringRef,
    _port: u32,
    read_stream: MutPtr<CFTypeRef>,
    write_stream: MutPtr<CFTypeRef>,
) {
    // Socket streams are not implemented yet. Report creation failure through
    // both out parameters so callers can follow their offline/error path.
    if !read_stream.is_null() {
        env.mem.write(read_stream, Ptr::null());
    }
    if !write_stream.is_null() {
        env.mem.write(write_stream, Ptr::null());
    }
    log!("CFStream socket streams unavailable; returning no streams");
}

pub const FUNCTIONS: FunctionExports = &[export_c_func!(CFStreamCreatePairWithSocketToHost(
    _,
    _,
    _,
    _,
    _
))];
