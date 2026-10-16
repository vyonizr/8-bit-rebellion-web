/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `UIActivityIndicatorView`.

use crate::frameworks::core_graphics::CGRect;
use crate::frameworks::foundation::NSInteger;
use crate::objc::{
    id, impl_HostObject_with_superclass, msg, msg_super, objc_classes, todo_objc_setter,
    ClassExports, NSZonePtr,
};

type UIActivityIndicatorViewStyle = NSInteger;

pub struct UIActivityIndicatorViewHostObject {
    superclass: super::ui_view::UIViewHostObject,
    animating: bool,
    hides_when_stopped: bool,
}
impl_HostObject_with_superclass!(UIActivityIndicatorViewHostObject);

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

@implementation UIActivityIndicatorView: UIView

+ (id)allocWithZone:(NSZonePtr)_zone {
    let host_object = Box::new(UIActivityIndicatorViewHostObject {
        superclass: Default::default(),
        animating: false,
        hides_when_stopped: true,
    });
    env.objc.alloc_object(this, host_object, &mut env.mem)
}

- (id)initWithActivityIndicatorStyle:(UIActivityIndicatorViewStyle)_style {
    // TODO: proper init
    msg![env; this init]
}

- (id)initWithFrame:(CGRect)frame {
    let this: id = msg_super![env; this initWithFrame:frame];
    () = msg![env; this setHidden:true];
    this
}

- (id)initWithCoder:(id)coder {
    let this: id = msg_super![env; this initWithCoder:coder];
    () = msg![env; this setHidden:true];
    this
}

- (())setActivityIndicatorViewStyle:(UIActivityIndicatorViewStyle)style {
    todo_objc_setter!(this, style);
}

- (())startAnimating {
    log!("TODO: [(UIActivityIndicatorView *){:?} startAnimating]", this);
    env.objc.borrow_mut::<UIActivityIndicatorViewHostObject>(this).animating = true;
    if env.objc.borrow::<UIActivityIndicatorViewHostObject>(this).hides_when_stopped {
        () = msg![env; this setHidden:false];
    }
}
- (())stopAnimating {
    log!("TODO: [(UIActivityIndicatorView *){:?} stopAnimating]", this);
    env.objc.borrow_mut::<UIActivityIndicatorViewHostObject>(this).animating = false;
    if env.objc.borrow::<UIActivityIndicatorViewHostObject>(this).hides_when_stopped {
        () = msg![env; this setHidden:true];
    }
}

- (bool)isAnimating {
    env.objc.borrow::<UIActivityIndicatorViewHostObject>(this).animating
}

- (bool)hidesWhenStopped {
    env.objc.borrow::<UIActivityIndicatorViewHostObject>(this).hides_when_stopped
}

- (())setHidesWhenStopped:(bool)hides {
    let animating = {
        let host = env.objc.borrow_mut::<UIActivityIndicatorViewHostObject>(this);
        host.hides_when_stopped = hides;
        host.animating
    };
    if !animating {
        () = msg![env; this setHidden:hides];
    }
}

@end

};
