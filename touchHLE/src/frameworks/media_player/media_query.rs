/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `MPMediaQuery`.

use crate::frameworks::foundation::ns_array;
use crate::objc::{
    autorelease, id, msg, msg_class, objc_classes, release, retain, ClassExports, HostObject,
    NSZonePtr,
};

#[derive(Default)]
struct MPMediaQueryHostObject {
    predicates: Vec<id>,
}
impl HostObject for MPMediaQueryHostObject {}

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

@implementation MPMediaQuery: NSObject

+ (id)allocWithZone:(NSZonePtr)_zone {
    env.objc
        .alloc_object(this, Box::new(MPMediaQueryHostObject::default()), &mut env.mem)
}

+ (id)playlistsQuery {
    let query: id = msg![env; this new];
    autorelease(env, query)
}

+ (id)songsQuery {
    let query: id = msg![env; this new];
    autorelease(env, query)
}

+ (id)artistsQuery {
    let query: id = msg![env; this new];
    autorelease(env, query)
}

- (())addFilterPredicate:(id)predicate {
    let already_present = env.objc.borrow::<MPMediaQueryHostObject>(this)
        .predicates.contains(&predicate);
    if !already_present {
        let predicate = retain(env, predicate);
        env.objc.borrow_mut::<MPMediaQueryHostObject>(this)
            .predicates.push(predicate);
    }
}

- (())removeFilterPredicate:(id)predicate {
    let state = env.objc.borrow_mut::<MPMediaQueryHostObject>(this);
    if let Some(index) = state.predicates.iter().position(|item| *item == predicate) {
        let removed = state.predicates.remove(index);
        release(env, removed);
    }
}

- (id)filterPredicates {
    let predicates = env.objc.borrow::<MPMediaQueryHostObject>(this).predicates.clone();
    let predicates = predicates.into_iter().map(|item| retain(env, item)).collect();
    let array = ns_array::from_vec(env, predicates);
    let set: id = msg_class![env; NSSet setWithArray:array];
    release(env, array);
    set
}

- (id)items {
    msg_class![env; NSArray array]
}

- (id)collections {
    msg_class![env; NSArray array]
}

- (())dealloc {
    let predicates = std::mem::take(&mut env.objc.borrow_mut::<MPMediaQueryHostObject>(this).predicates);
    for predicate in predicates {
        release(env, predicate);
    }
    env.objc.dealloc_object(this, &mut env.mem);
}

@end

};
