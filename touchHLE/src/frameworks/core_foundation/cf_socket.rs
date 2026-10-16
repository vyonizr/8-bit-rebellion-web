/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `CFSocket`
//!
//! Only sockets wrapping the native handle of a socket stream (see
//! [super::cf_stream]) are supported, which is how AsyncSocket inspects a
//! connection it made with `CFStreamCreatePairWithSocketToHost`. The real
//! connection belongs to the host, so the handle and addresses are stand-ins.

use super::cf_allocator::{kCFAllocatorDefault, CFAllocatorRef};
use super::cf_data::{CFDataCreate, CFDataRef};
use super::CFTypeRef;
use crate::dyld::{export_c_func, FunctionExports};
use crate::mem::{MutVoidPtr, Ptr};
use crate::objc::{nil, objc_classes, ClassExports, HostObject};
use crate::Environment;

type CFSocketRef = CFTypeRef;
type CFSocketNativeHandle = i32;

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

@implementation _touchHLE_CFSocket: NSObject
@end

};

struct CFSocketHostObject {
    native: CFSocketNativeHandle,
    valid: bool,
}
impl HostObject for CFSocketHostObject {}

/// Create a `CFData` holding a copy of `bytes`.
pub fn data_from_bytes(env: &mut Environment, bytes: &[u8]) -> CFDataRef {
    let len = bytes.len() as u32;
    let tmp = env.mem.alloc(len).cast::<u8>();
    env.mem.bytes_at_mut(tmp, len).copy_from_slice(bytes);
    let data = CFDataCreate(env, kCFAllocatorDefault, tmp.cast_const(), len as _);
    env.mem.free(tmp.cast());
    data
}

/// A BSD `sockaddr_in` for 127.0.0.1:80. AsyncSocket only checks the family.
fn loopback_address(env: &mut Environment) -> CFDataRef {
    let mut addr = [0u8; 16];
    addr[0] = 16; // sin_len
    addr[1] = 2; // sin_family = AF_INET
    addr[2..4].copy_from_slice(&80u16.to_be_bytes());
    addr[4..8].copy_from_slice(&[127, 0, 0, 1]);
    data_from_bytes(env, &addr)
}

fn CFSocketCreate(
    env: &mut Environment,
    allocator: CFAllocatorRef,
    protocol_family: i32,
    type_: i32,
    protocol: i32,
    flags: u32,
    callout: MutVoidPtr,
    context: MutVoidPtr,
) -> CFTypeRef {
    assert!(allocator == kCFAllocatorDefault || env.mem.read(allocator).is_system_default()); // unimplemented
    log!(
        "TODO: CFSocketCreate({}, {}, {}, {}, {:?}, {:?}) -> NULL",
        protocol_family,
        type_,
        protocol,
        flags,
        callout,
        context
    );
    Ptr::null()
}

fn CFSocketCreateWithNative(
    env: &mut Environment,
    _allocator: CFAllocatorRef,
    native: CFSocketNativeHandle,
    _callback_types: u32,
    _callout: MutVoidPtr,
    _context: MutVoidPtr,
) -> CFSocketRef {
    let host_object = Box::new(CFSocketHostObject {
        native,
        valid: true,
    });
    let class = env.objc.get_known_class("_touchHLE_CFSocket", &mut env.mem);
    env.objc.alloc_object(class, host_object, &mut env.mem)
}

fn CFSocketCopyPeerAddress(env: &mut Environment, _socket: CFSocketRef) -> CFDataRef {
    loopback_address(env)
}

fn CFSocketCopyAddress(env: &mut Environment, _socket: CFSocketRef) -> CFDataRef {
    loopback_address(env)
}

fn CFSocketGetNative(env: &mut Environment, socket: CFSocketRef) -> CFSocketNativeHandle {
    if socket == nil {
        return -1;
    }
    env.objc.borrow::<CFSocketHostObject>(socket).native
}

fn CFSocketIsValid(env: &mut Environment, socket: CFSocketRef) -> bool {
    env.objc.borrow::<CFSocketHostObject>(socket).valid
}

fn CFSocketInvalidate(env: &mut Environment, socket: CFSocketRef) {
    env.objc.borrow_mut::<CFSocketHostObject>(socket).valid = false;
}

fn CFSocketSetSocketFlags(_env: &mut Environment, _socket: CFSocketRef, _flags: u32) {}

pub const FUNCTIONS: FunctionExports = &[
    export_c_func!(CFSocketCreate(_, _, _, _, _, _, _)),
    export_c_func!(CFSocketCreateWithNative(_, _, _, _, _)),
    export_c_func!(CFSocketCopyPeerAddress(_)),
    export_c_func!(CFSocketCopyAddress(_)),
    export_c_func!(CFSocketGetNative(_)),
    export_c_func!(CFSocketIsValid(_)),
    export_c_func!(CFSocketInvalidate(_)),
    export_c_func!(CFSocketSetSocketFlags(_, _)),
];
