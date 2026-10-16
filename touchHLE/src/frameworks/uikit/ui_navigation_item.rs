/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `UINavigationItem`.

use crate::objc::{id, nil, objc_classes, release, retain, ClassExports, HostObject, NSZonePtr};

#[derive(Default)]
struct UINavigationItemHostObject {
    title: id,
    left_bar_button_item: id,
    right_bar_button_item: id,
}
impl HostObject for UINavigationItemHostObject {}

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

@implementation UINavigationItem: NSObject

+ (id)allocWithZone:(NSZonePtr)_zone {
    let host_object = Box::<UINavigationItemHostObject>::default();
    env.objc.alloc_object(this, host_object, &mut env.mem)
}

- (id)initWithTitle:(id)title {
    retain(env, title);
    env.objc.borrow_mut::<UINavigationItemHostObject>(this).title = title;
    this
}

- (id)initWithCoder:(id)_coder {
    // TODO: Decode archived navigation item properties.
    this
}

- (id)title {
    env.objc.borrow::<UINavigationItemHostObject>(this).title
}

- (())setTitle:(id)title {
    retain(env, title);
    let old = std::mem::replace(&mut env.objc.borrow_mut::<UINavigationItemHostObject>(this).title, title);
    release(env, old);
}

- (id)leftBarButtonItem {
    env.objc.borrow::<UINavigationItemHostObject>(this).left_bar_button_item
}

- (())setLeftBarButtonItem:(id)item {
    retain(env, item);
    let old = std::mem::replace(&mut env.objc.borrow_mut::<UINavigationItemHostObject>(this).left_bar_button_item, item);
    release(env, old);
}

- (id)rightBarButtonItem {
    env.objc.borrow::<UINavigationItemHostObject>(this).right_bar_button_item
}

- (())setRightBarButtonItem:(id)item {
    retain(env, item);
    let old = std::mem::replace(&mut env.objc.borrow_mut::<UINavigationItemHostObject>(this).right_bar_button_item, item);
    release(env, old);
}

- (())dealloc {
    let host_object = env.objc.borrow_mut::<UINavigationItemHostObject>(this);
    let title = std::mem::replace(&mut host_object.title, nil);
    let left = std::mem::replace(&mut host_object.left_bar_button_item, nil);
    let right = std::mem::replace(&mut host_object.right_bar_button_item, nil);
    release(env, title);
    release(env, left);
    release(env, right);
    env.objc.dealloc_object(this, &mut env.mem)
}

@end

};
