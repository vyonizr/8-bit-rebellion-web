/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `MPMediaPropertyPredicate` for an emulated empty media library.

use crate::objc::{
    autorelease, id, nil, objc_classes, release, retain, ClassExports, HostObject, NSZonePtr,
};

struct MPMediaPropertyPredicateHostObject {
    value: id,
    property: id,
}
impl HostObject for MPMediaPropertyPredicateHostObject {}

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

@implementation MPMediaPropertyPredicate: NSObject

+ (id)allocWithZone:(NSZonePtr)_zone {
    env.objc.alloc_object(
        this,
        Box::new(MPMediaPropertyPredicateHostObject {
            value: nil,
            property: nil,
        }),
        &mut env.mem,
    )
}

+ (id)predicateWithValue:(id)value forProperty:(id)property {
    let value = retain(env, value);
    let property = retain(env, property);
    let result = env.objc.alloc_object(
        this,
        Box::new(MPMediaPropertyPredicateHostObject {
            value,
            property,
        }),
        &mut env.mem,
    );
    autorelease(env, result)
}

- (id)value {
    env.objc.borrow::<MPMediaPropertyPredicateHostObject>(this).value
}

- (id)property {
    env.objc.borrow::<MPMediaPropertyPredicateHostObject>(this).property
}

- (())dealloc {
    let state = env.objc.borrow::<MPMediaPropertyPredicateHostObject>(this);
    let (value, property) = (state.value, state.property);
    release(env, value);
    release(env, property);
    env.objc.dealloc_object(this, &mut env.mem);
}

@end

};
