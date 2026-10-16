/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `UINavigationBar`.

use crate::frameworks::foundation::NSUInteger;
use crate::objc::{
    id, impl_HostObject_with_superclass, msg, msg_super, nil, objc_classes, release, ClassExports,
    NSZonePtr,
};

#[derive(Default)]
struct UINavigationBarHostObject {
    superclass: super::UIViewHostObject,
    items: id,
}
impl_HostObject_with_superclass!(UINavigationBarHostObject);

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

@implementation UINavigationBar: UIView

+ (id)allocWithZone:(NSZonePtr)_zone {
    let host_object = Box::<UINavigationBarHostObject>::default();
    env.objc.alloc_object(this, host_object, &mut env.mem)
}

- (id)items {
    env.objc.borrow::<UINavigationBarHostObject>(this).items
}

- (())setItems:(id)items {
    let copy: id = msg![env; items copy];
    let old = std::mem::replace(&mut env.objc.borrow_mut::<UINavigationBarHostObject>(this).items, copy);
    release(env, old);
}

- (())setItems:(id)items animated:(bool)_animated {
    () = msg![env; this setItems:items];
}

- (id)topItem {
    let items: id = msg![env; this items];
    let count: NSUInteger = msg![env; items count];
    if count == 0 { nil } else { msg![env; items objectAtIndex:(count - 1)] }
}

- (id)backItem {
    let items: id = msg![env; this items];
    let count: NSUInteger = msg![env; items count];
    if count < 2 { nil } else { msg![env; items objectAtIndex:(count - 2)] }
}

- (())dealloc {
    let items = std::mem::replace(&mut env.objc.borrow_mut::<UINavigationBarHostObject>(this).items, nil);
    release(env, items);
    msg_super![env; this dealloc]
}

@end

};
