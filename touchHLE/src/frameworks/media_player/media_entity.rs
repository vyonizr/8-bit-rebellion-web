/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `MPMediaEntity`.

use crate::dyld::{ConstantExports, HostConstant};
use crate::objc::{objc_classes, ClassExports};

pub const CONSTANTS: ConstantExports = &[
    (
        "_MPMediaItemPropertyArtist",
        HostConstant::NSString("artist"),
    ),
    ("_MPMediaItemPropertyTitle", HostConstant::NSString("title")),
    (
        "_MPMediaItemPropertyAlbumTitle",
        HostConstant::NSString("albumTitle"),
    ),
];

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

@implementation MPMediaEntity: NSObject
// TODO
@end

};
