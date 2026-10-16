/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `UIBarItem` and `UIBarButtonItem`.

use crate::frameworks::foundation::NSInteger;
use crate::objc::{
    id, impl_HostObject_with_superclass, nil, objc_classes, release, retain, ClassExports,
    HostObject, NSZonePtr, SEL,
};

#[derive(Default)]
struct UIBarItemHostObject {
    title: id,
}
impl HostObject for UIBarItemHostObject {}

#[derive(Default)]
struct UIBarButtonItemHostObject {
    superclass: UIBarItemHostObject,
    style: NSInteger,
    target: id, // weak
    action: Option<SEL>,
}
impl_HostObject_with_superclass!(UIBarButtonItemHostObject);

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

@implementation UIBarItem: NSObject

+ (id)allocWithZone:(NSZonePtr)_zone {
    let host_object = Box::<UIBarItemHostObject>::default();
    env.objc.alloc_object(this, host_object, &mut env.mem)
}

- (id)title {
    env.objc.borrow::<UIBarItemHostObject>(this).title
}

- (())setTitle:(id)title {
    retain(env, title);
    let old = std::mem::replace(&mut env.objc.borrow_mut::<UIBarItemHostObject>(this).title, title);
    release(env, old);
}

- (())dealloc {
    let title = std::mem::replace(&mut env.objc.borrow_mut::<UIBarItemHostObject>(this).title, nil);
    release(env, title);
    env.objc.dealloc_object(this, &mut env.mem)
}

@end

@implementation UIBarButtonItem: UIBarItem

+ (id)allocWithZone:(NSZonePtr)_zone {
    let host_object = Box::<UIBarButtonItemHostObject>::default();
    env.objc.alloc_object(this, host_object, &mut env.mem)
}

- (id)initWithTitle:(id)title
             style:(NSInteger)style
            target:(id)target
            action:(SEL)action {
    () = crate::msg![env; this setTitle:title];
    let host_object = env.objc.borrow_mut::<UIBarButtonItemHostObject>(this);
    host_object.style = style;
    host_object.target = target;
    host_object.action = Some(action);
    this
}

- (id)target {
    env.objc.borrow::<UIBarButtonItemHostObject>(this).target
}

- (())setTarget:(id)target {
    env.objc.borrow_mut::<UIBarButtonItemHostObject>(this).target = target;
}

@end

};
