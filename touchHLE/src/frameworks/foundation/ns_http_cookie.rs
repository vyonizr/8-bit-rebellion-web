/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! HTTP cookie property keys.

use crate::dyld::{ConstantExports, HostConstant};

pub const CONSTANTS: ConstantExports = &[
    ("_NSHTTPCookieName", HostConstant::NSString("Name")),
    ("_NSHTTPCookieValue", HostConstant::NSString("Value")),
    ("_NSHTTPCookieDomain", HostConstant::NSString("Domain")),
    ("_NSHTTPCookiePath", HostConstant::NSString("Path")),
];
