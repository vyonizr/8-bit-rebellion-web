/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `CFReadStream` and `CFWriteStream` for TCP sockets.
//!
//! Only socket streams from `CFStreamCreatePairWithSocketToHost` are
//! supported, which is what AsyncSocket uses. The connection is made by the
//! host:
//!
//! - In a browser, it is tunnelled through a WebSocket to the server that
//!   hosts the page (`/net`, see `web/touchhle_lib.js` and `server/`).
//! - Natively, it is a TCP connection to the address in the
//!   `TOUCHHLE_GAME_SERVER` environment variable (`host:port`), or to the
//!   requested address if `--allow-network-access` is set.
//!
//! Events are delivered to the stream's client callback from the run loop the
//! stream is scheduled on ([handle_stream]).

use super::cf_allocator::CFAllocatorRef;
use super::cf_string::CFStringRef;
use super::{CFIndex, CFTypeRef};
use crate::abi::{impl_GuestRet_for_large_struct, CallFromHost, GuestFunction};
use crate::dyld::{export_c_func, ConstantExports, FunctionExports, HostConstant};
use crate::frameworks::foundation::{ns_run_loop, ns_string};
use crate::mem::{ConstPtr, ConstVoidPtr, MutPtr, MutVoidPtr, Ptr, SafeRead};
use crate::objc::{id, nil, objc_classes, release, retain, ClassExports, HostObject};
use crate::Environment;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

pub type CFReadStreamRef = CFTypeRef;
pub type CFWriteStreamRef = CFTypeRef;
type CFStreamEventType = u32;
type CFStreamStatus = CFIndex;

const kCFStreamEventOpenCompleted: CFStreamEventType = 1;
const kCFStreamEventHasBytesAvailable: CFStreamEventType = 2;
const kCFStreamEventCanAcceptBytes: CFStreamEventType = 4;
const kCFStreamEventErrorOccurred: CFStreamEventType = 8;
const kCFStreamEventEndEncountered: CFStreamEventType = 16;

const kCFStreamStatusNotOpen: CFStreamStatus = 0;
const kCFStreamStatusOpening: CFStreamStatus = 1;
const kCFStreamStatusOpen: CFStreamStatus = 2;
const kCFStreamStatusAtEnd: CFStreamStatus = 5;
const kCFStreamStatusClosed: CFStreamStatus = 6;
const kCFStreamStatusError: CFStreamStatus = 7;

const kCFStreamErrorDomainPOSIX: CFIndex = 1;
const ECONNREFUSED: i32 = 61;

#[derive(Copy, Clone, Debug, Default)]
#[repr(C, packed)]
pub struct CFStreamError {
    domain: CFIndex,
    error: i32,
}
unsafe impl SafeRead for CFStreamError {}
impl_GuestRet_for_large_struct!(CFStreamError);

/// `CFStreamClientContext`
#[derive(Copy, Clone)]
#[repr(C, packed)]
struct CFStreamClientContext {
    version: CFIndex,
    info: MutVoidPtr,
    retain: ConstVoidPtr,
    release: ConstVoidPtr,
    copy_description: ConstVoidPtr,
}
unsafe impl SafeRead for CFStreamClientContext {}

#[derive(Copy, Clone, PartialEq, Debug)]
enum TransportState {
    Connecting,
    Open,
    /// The peer closed the connection.
    Closed,
    Error,
}

#[cfg(target_os = "emscripten")]
mod transport {
    use super::TransportState;
    use std::collections::VecDeque;

    extern "C" {
        fn touchhle_net_open(host: *const u8, host_len: usize, port: u32) -> i32;
        fn touchhle_net_state(id: i32) -> i32;
        fn touchhle_net_recv(id: i32, buf: *mut u8, len: usize) -> usize;
        fn touchhle_net_send(id: i32, buf: *const u8, len: usize);
        fn touchhle_net_close(id: i32);
    }

    pub struct Transport(i32);
    impl Transport {
        pub fn connect(host: &str, port: u16, _network_access: bool) -> Transport {
            Transport(unsafe { touchhle_net_open(host.as_ptr(), host.len(), port.into()) })
        }
        pub fn poll(&mut self, received: &mut VecDeque<u8>) -> TransportState {
            let mut buf = [0u8; 4096];
            loop {
                let n = unsafe { touchhle_net_recv(self.0, buf.as_mut_ptr(), buf.len()) };
                if n == 0 {
                    break;
                }
                received.extend(&buf[..n]);
            }
            match unsafe { touchhle_net_state(self.0) } {
                0 => TransportState::Connecting,
                1 => TransportState::Open,
                2 => TransportState::Closed,
                _ => TransportState::Error,
            }
        }
        pub fn send(&mut self, data: &[u8]) {
            unsafe { touchhle_net_send(self.0, data.as_ptr(), data.len()) }
        }
    }
    impl Drop for Transport {
        fn drop(&mut self) {
            unsafe { touchhle_net_close(self.0) }
        }
    }
}

#[cfg(not(target_os = "emscripten"))]
mod transport {
    use super::TransportState;
    use std::collections::VecDeque;
    use std::io::{ErrorKind, Read, Write};
    use std::net::TcpStream;

    pub struct Transport {
        stream: Option<TcpStream>,
        state: TransportState,
    }
    impl Transport {
        pub fn connect(host: &str, port: u16, network_access: bool) -> Transport {
            let target = std::env::var("TOUCHHLE_GAME_SERVER")
                .ok()
                .or_else(|| network_access.then(|| format!("{host}:{port}")));
            let Some(target) = target else {
                log!("Network access is disabled; refusing connection to {host}:{port}");
                return Transport {
                    stream: None,
                    state: TransportState::Error,
                };
            };
            log!("Connecting to {target} (requested {host}:{port})");
            // ponytail: blocking connect; fine for a local or nearby server.
            match TcpStream::connect(&target).and_then(|s| s.set_nonblocking(true).map(|_| s)) {
                Ok(stream) => Transport {
                    stream: Some(stream),
                    state: TransportState::Open,
                },
                Err(e) => {
                    log!("Connection to {target} failed: {e}");
                    Transport {
                        stream: None,
                        state: TransportState::Error,
                    }
                }
            }
        }
        pub fn poll(&mut self, received: &mut VecDeque<u8>) -> TransportState {
            if let (Some(stream), TransportState::Open) = (&mut self.stream, self.state) {
                let mut buf = [0u8; 4096];
                loop {
                    match stream.read(&mut buf) {
                        Ok(0) => {
                            self.state = TransportState::Closed;
                            break;
                        }
                        Ok(n) => received.extend(&buf[..n]),
                        Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                        Err(e) => {
                            log!("Socket read failed: {e}");
                            self.state = TransportState::Error;
                            break;
                        }
                    }
                }
            }
            self.state
        }
        pub fn send(&mut self, mut data: &[u8]) {
            let Some(stream) = &mut self.stream else {
                return;
            };
            // ponytail: spins until the kernel accepts all data; messages are
            // small, add a send queue if large writes ever stall.
            while !data.is_empty() {
                match stream.write(data) {
                    Ok(n) => data = &data[n..],
                    Err(e) if e.kind() == ErrorKind::WouldBlock => continue,
                    Err(e) => {
                        log!("Socket write failed: {e}");
                        self.state = TransportState::Error;
                        return;
                    }
                }
            }
        }
    }
}

/// Whether socket streams reach a game server without general network
/// access: always in a browser, and natively when `TOUCHHLE_GAME_SERVER` is set.
pub fn game_server_available() -> bool {
    cfg!(target_os = "emscripten") || std::env::var_os("TOUCHHLE_GAME_SERVER").is_some()
}

/// The connection shared by a read stream and its write stream.
struct Connection {
    transport: transport::Transport,
    received: VecDeque<u8>,
    state: TransportState,
}

struct SocketStreamHostObject {
    is_read: bool,
    connection: Rc<RefCell<Connection>>,
    status: CFStreamStatus,
    client: Option<(CFStreamEventType, GuestFunction, MutVoidPtr)>,
    run_loop: id,
    /// Events already delivered that shouldn't repeat.
    delivered: CFStreamEventType,
}
impl HostObject for SocketStreamHostObject {}

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

// Stands in for both CFReadStream and CFWriteStream socket streams.
@implementation _touchHLE_CFSocketStream: NSObject
@end

};

fn new_stream(env: &mut Environment, is_read: bool, connection: Rc<RefCell<Connection>>) -> id {
    let host_object = Box::new(SocketStreamHostObject {
        is_read,
        connection,
        status: kCFStreamStatusNotOpen,
        client: None,
        run_loop: nil,
        delivered: 0,
    });
    let class = env
        .objc
        .get_known_class("_touchHLE_CFSocketStream", &mut env.mem);
    env.objc.alloc_object(class, host_object, &mut env.mem)
}

fn CFStreamCreatePairWithSocketToHost(
    env: &mut Environment,
    _allocator: CFAllocatorRef,
    host: CFStringRef,
    port: u32,
    read_stream: MutPtr<CFReadStreamRef>,
    write_stream: MutPtr<CFWriteStreamRef>,
) {
    let host = ns_string::to_rust_string(env, host).to_string();
    let connection = Rc::new(RefCell::new(Connection {
        transport: transport::Transport::connect(&host, port as u16, env.options.network_access),
        received: VecDeque::new(),
        state: TransportState::Connecting,
    }));
    if !read_stream.is_null() {
        let stream = new_stream(env, true, connection.clone());
        env.mem.write(read_stream, stream);
    }
    if !write_stream.is_null() {
        let stream = new_stream(env, false, connection);
        env.mem.write(write_stream, stream);
    }
}

fn host_object(env: &mut Environment, stream: CFTypeRef) -> &mut SocketStreamHostObject {
    env.objc.borrow_mut::<SocketStreamHostObject>(stream)
}

fn set_client(
    env: &mut Environment,
    stream: CFTypeRef,
    events: CFStreamEventType,
    callback: GuestFunction,
    context: ConstPtr<CFStreamClientContext>,
) -> bool {
    let info = if context.is_null() {
        Ptr::null()
    } else {
        env.mem.read(context).info
    };
    host_object(env, stream).client =
        (callback.addr_with_thumb_bit() != 0).then_some((events, callback, info));
    true
}

fn schedule(env: &mut Environment, stream: CFTypeRef, run_loop: id) {
    let old = std::mem::replace(&mut host_object(env, stream).run_loop, run_loop);
    if old == nil {
        ns_run_loop::add_stream(env, run_loop, stream);
    }
}

fn unschedule(env: &mut Environment, stream: CFTypeRef) {
    let old = std::mem::replace(&mut host_object(env, stream).run_loop, nil);
    if old != nil {
        ns_run_loop::remove_stream(env, old, stream);
    }
}

fn open(env: &mut Environment, stream: CFTypeRef) -> bool {
    host_object(env, stream).status = kCFStreamStatusOpening;
    true
}

fn close(env: &mut Environment, stream: CFTypeRef) {
    host_object(env, stream).status = kCFStreamStatusClosed;
    unschedule(env, stream);
}

fn get_error(env: &mut Environment, stream: CFTypeRef) -> CFStreamError {
    if host_object(env, stream).status == kCFStreamStatusError {
        CFStreamError {
            domain: kCFStreamErrorDomainPOSIX,
            error: ECONNREFUSED,
        }
    } else {
        CFStreamError::default()
    }
}

/// Poll a stream's connection and deliver any events to its client. Called by
/// the run loop the stream is scheduled on.
pub fn handle_stream(env: &mut Environment, stream: CFTypeRef) {
    let mut to_deliver = Vec::new();
    {
        let host = host_object(env, stream);
        let (state, has_bytes) = {
            let mut guard = host.connection.borrow_mut();
            let connection = &mut *guard;
            connection.state = connection.transport.poll(&mut connection.received);
            (connection.state, !connection.received.is_empty())
        };

        if host.status == kCFStreamStatusOpening && state != TransportState::Connecting {
            host.status = kCFStreamStatusOpen;
            to_deliver.push(kCFStreamEventOpenCompleted);
        }
        if host.status == kCFStreamStatusOpen {
            if host.is_read && has_bytes {
                to_deliver.push(kCFStreamEventHasBytesAvailable);
            } else if !host.is_read && host.delivered & kCFStreamEventCanAcceptBytes == 0 {
                host.delivered |= kCFStreamEventCanAcceptBytes;
                to_deliver.push(kCFStreamEventCanAcceptBytes);
            }
            if state == TransportState::Error {
                host.status = kCFStreamStatusError;
                to_deliver.push(kCFStreamEventErrorOccurred);
            } else if state == TransportState::Closed && host.is_read && !has_bytes {
                // Like TCP, only the read side sees the peer closing.
                host.status = kCFStreamStatusAtEnd;
                to_deliver.push(kCFStreamEventEndEncountered);
            }
        }
    }
    for event in to_deliver {
        let Some((events, callback, info)) = host_object(env, stream).client else {
            return;
        };
        if events & event != 0 {
            log_dbg!("Stream {:?} event {}", stream, event);
            // The callback might release the stream, so keep it alive.
            retain(env, stream);
            let () = callback.call_from_host(env, (stream, event, info));
            release(env, stream);
        }
    }
}

fn CFReadStreamSetClient(
    env: &mut Environment,
    stream: CFReadStreamRef,
    events: CFStreamEventType,
    callback: GuestFunction,
    context: ConstPtr<CFStreamClientContext>,
) -> bool {
    set_client(env, stream, events, callback, context)
}
fn CFWriteStreamSetClient(
    env: &mut Environment,
    stream: CFWriteStreamRef,
    events: CFStreamEventType,
    callback: GuestFunction,
    context: ConstPtr<CFStreamClientContext>,
) -> bool {
    set_client(env, stream, events, callback, context)
}
fn CFReadStreamScheduleWithRunLoop(env: &mut Environment, stream: CFReadStreamRef, run_loop: id, _mode: id) {
    schedule(env, stream, run_loop)
}
fn CFWriteStreamScheduleWithRunLoop(env: &mut Environment, stream: CFWriteStreamRef, run_loop: id, _mode: id) {
    schedule(env, stream, run_loop)
}
fn CFReadStreamUnscheduleFromRunLoop(env: &mut Environment, stream: CFReadStreamRef, _run_loop: id, _mode: id) {
    unschedule(env, stream)
}
fn CFWriteStreamUnscheduleFromRunLoop(env: &mut Environment, stream: CFWriteStreamRef, _run_loop: id, _mode: id) {
    unschedule(env, stream)
}
fn CFReadStreamOpen(env: &mut Environment, stream: CFReadStreamRef) -> bool {
    open(env, stream)
}
fn CFWriteStreamOpen(env: &mut Environment, stream: CFWriteStreamRef) -> bool {
    open(env, stream)
}
fn CFReadStreamClose(env: &mut Environment, stream: CFReadStreamRef) {
    close(env, stream)
}
fn CFWriteStreamClose(env: &mut Environment, stream: CFWriteStreamRef) {
    close(env, stream)
}
fn CFReadStreamGetStatus(env: &mut Environment, stream: CFReadStreamRef) -> CFStreamStatus {
    host_object(env, stream).status
}
fn CFWriteStreamGetStatus(env: &mut Environment, stream: CFWriteStreamRef) -> CFStreamStatus {
    host_object(env, stream).status
}
fn CFReadStreamGetError(env: &mut Environment, stream: CFReadStreamRef) -> CFStreamError {
    get_error(env, stream)
}
fn CFWriteStreamGetError(env: &mut Environment, stream: CFWriteStreamRef) -> CFStreamError {
    get_error(env, stream)
}

fn CFReadStreamHasBytesAvailable(env: &mut Environment, stream: CFReadStreamRef) -> bool {
    !host_object(env, stream)
        .connection
        .borrow()
        .received
        .is_empty()
}

fn CFReadStreamRead(
    env: &mut Environment,
    stream: CFReadStreamRef,
    buffer: MutPtr<u8>,
    length: CFIndex,
) -> CFIndex {
    let host = host_object(env, stream);
    if host.status == kCFStreamStatusError {
        return -1;
    }
    let connection = host.connection.clone();
    let mut connection = connection.borrow_mut();
    let count = (length.max(0) as usize).min(connection.received.len());
    let data: Vec<u8> = connection.received.drain(..count).collect();
    drop(connection);
    env.mem
        .bytes_at_mut(buffer, count as u32)
        .copy_from_slice(&data);
    count as CFIndex
}

fn CFWriteStreamCanAcceptBytes(env: &mut Environment, stream: CFWriteStreamRef) -> bool {
    host_object(env, stream).status == kCFStreamStatusOpen
}

fn CFWriteStreamWrite(
    env: &mut Environment,
    stream: CFWriteStreamRef,
    buffer: ConstPtr<u8>,
    length: CFIndex,
) -> CFIndex {
    if host_object(env, stream).status != kCFStreamStatusOpen {
        return -1;
    }
    let data = env.mem.bytes_at(buffer, length as u32).to_vec();
    let host = host_object(env, stream);
    // Writes complete immediately, so signal readiness again.
    host.delivered &= !kCFStreamEventCanAcceptBytes;
    host.connection.borrow_mut().transport.send(&data);
    length
}

fn CFReadStreamCopyProperty(env: &mut Environment, stream: CFReadStreamRef, key: CFStringRef) -> CFTypeRef {
    let key = ns_string::to_rust_string(env, key);
    if key != "kCFStreamPropertySocketNativeHandle" {
        log_dbg!("CFReadStreamCopyProperty({}) -> NULL", key);
        return nil;
    }
    // AsyncSocket wraps this in a CFSocket (see cf_socket.rs). The host owns
    // the real socket, so any distinct number will do.
    let handle = 1000 + (stream.to_bits() >> 4) as i32;
    super::cf_socket::data_from_bytes(env, &handle.to_le_bytes())
}

fn CFReadStreamSetProperty(_env: &mut Environment, _stream: CFReadStreamRef, _key: CFStringRef, _value: CFTypeRef) -> bool {
    true
}
fn CFWriteStreamSetProperty(_env: &mut Environment, _stream: CFWriteStreamRef, _key: CFStringRef, _value: CFTypeRef) -> bool {
    true
}

/// Allocate a guest integer constant, for `CFIndex`-typed exported constants.
fn index_constant(env: &mut Environment, value: CFIndex) -> ConstVoidPtr {
    let ptr = env.mem.alloc(4).cast::<CFIndex>();
    env.mem.write(ptr, value);
    ptr.cast_void().cast_const()
}

pub const CONSTANTS: ConstantExports = &[
    (
        "_kCFStreamPropertySocketNativeHandle",
        HostConstant::NSString("kCFStreamPropertySocketNativeHandle"),
    ),
    (
        "_kCFStreamPropertyShouldCloseNativeSocket",
        HostConstant::NSString("kCFStreamPropertyShouldCloseNativeSocket"),
    ),
    // Values from CFNetwork's CFHost.h/CFSocketStream.h.
    ("_kCFStreamErrorDomainSSL", HostConstant::Custom(|env| index_constant(env, 3))),
    ("_kCFStreamErrorDomainNetServices", HostConstant::Custom(|env| index_constant(env, 10))),
    ("_kCFStreamErrorDomainMach", HostConstant::Custom(|env| index_constant(env, 11))),
    ("_kCFStreamErrorDomainNetDB", HostConstant::Custom(|env| index_constant(env, 12))),
    ("_kCFStreamErrorDomainSOCKS", HostConstant::Custom(|env| index_constant(env, 13))),
    ("_kCFStreamErrorDomainSystemConfiguration", HostConstant::Custom(|env| index_constant(env, 17))),
];

pub const FUNCTIONS: FunctionExports = &[
    export_c_func!(CFStreamCreatePairWithSocketToHost(_, _, _, _, _)),
    export_c_func!(CFReadStreamSetClient(_, _, _, _)),
    export_c_func!(CFWriteStreamSetClient(_, _, _, _)),
    export_c_func!(CFReadStreamScheduleWithRunLoop(_, _, _)),
    export_c_func!(CFWriteStreamScheduleWithRunLoop(_, _, _)),
    export_c_func!(CFReadStreamUnscheduleFromRunLoop(_, _, _)),
    export_c_func!(CFWriteStreamUnscheduleFromRunLoop(_, _, _)),
    export_c_func!(CFReadStreamOpen(_)),
    export_c_func!(CFWriteStreamOpen(_)),
    export_c_func!(CFReadStreamClose(_)),
    export_c_func!(CFWriteStreamClose(_)),
    export_c_func!(CFReadStreamGetStatus(_)),
    export_c_func!(CFWriteStreamGetStatus(_)),
    export_c_func!(CFReadStreamGetError(_)),
    export_c_func!(CFWriteStreamGetError(_)),
    export_c_func!(CFReadStreamHasBytesAvailable(_)),
    export_c_func!(CFReadStreamRead(_, _, _)),
    export_c_func!(CFWriteStreamCanAcceptBytes(_)),
    export_c_func!(CFWriteStreamWrite(_, _, _)),
    export_c_func!(CFReadStreamCopyProperty(_, _)),
    export_c_func!(CFReadStreamSetProperty(_, _, _)),
    export_c_func!(CFWriteStreamSetProperty(_, _, _)),
];
