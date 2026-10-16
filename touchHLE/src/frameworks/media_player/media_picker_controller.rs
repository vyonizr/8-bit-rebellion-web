/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `MPMediaPickerController`.

use crate::frameworks::uikit::ui_view_controller::UIViewControllerHostObject;
use crate::objc::{
    id, impl_HostObject_with_superclass, msg_super, nil, objc_classes, release, retain,
    ClassExports, NSZonePtr,
};

#[derive(Default)]
struct MPMediaPickerControllerHostObject {
    superclass: UIViewControllerHostObject,
    // Delegates are weak references in UIKit.
    delegate: id,
    allows_picking_multiple_items: bool,
    prompt: id,
}
impl_HostObject_with_superclass!(MPMediaPickerControllerHostObject);

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

@implementation MPMediaPickerController: UIViewController

+ (id)allocWithZone:(NSZonePtr)_zone {
    env.objc.alloc_object(
        this,
        Box::<MPMediaPickerControllerHostObject>::default(),
        &mut env.mem,
    )
}

- (id)initWithMediaTypes:(u32)_media_types {
    msg_super![env; this init]
}

- (())setDelegate:(id)delegate {
    env.objc.borrow_mut::<MPMediaPickerControllerHostObject>(this).delegate = delegate;
}

- (id)delegate {
    env.objc.borrow::<MPMediaPickerControllerHostObject>(this).delegate
}

- (())setAllowsPickingMultipleItems:(bool)allows {
    env.objc.borrow_mut::<MPMediaPickerControllerHostObject>(this)
        .allows_picking_multiple_items = allows;
}

- (bool)allowsPickingMultipleItems {
    env.objc.borrow::<MPMediaPickerControllerHostObject>(this)
        .allows_picking_multiple_items
}

- (())setPrompt:(id)prompt {
    let prompt = retain(env, prompt);
    let state = env.objc.borrow_mut::<MPMediaPickerControllerHostObject>(this);
    let old_prompt = std::mem::replace(&mut state.prompt, prompt);
    release(env, old_prompt);
}

- (id)prompt {
    env.objc.borrow::<MPMediaPickerControllerHostObject>(this).prompt
}

- (())dealloc {
    let prompt = env.objc.borrow::<MPMediaPickerControllerHostObject>(this).prompt;
    if prompt != nil {
        release(env, prompt);
    }
    let _: () = msg_super![env; this dealloc];
}

@end

};
