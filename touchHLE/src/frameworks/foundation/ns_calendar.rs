/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `NSCalendar`.

use super::ns_date::NSDateHostObject;
use super::ns_date_components::NSDateComponentsHostObject;
use super::ns_string::to_rust_string;
use super::NSUInteger;
use crate::dyld::{ConstantExports, HostConstant};
use crate::frameworks::core_foundation::time::SECS_FROM_UNIX_TO_APPLE_EPOCHS;
use crate::libc::time::timestamp_to_calendar_date;
use crate::objc::{
    autorelease, id, msg_class, nil, objc_classes, release, retain, ClassExports, HostObject,
    NSZonePtr,
};

pub const CONSTANTS: ConstantExports =
    &[("_NSGregorianCalendar", HostConstant::NSString("gregorian"))];

#[derive(Default)]
struct NSCalendarHostObject {
    identifier: id,
}
impl HostObject for NSCalendarHostObject {}

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

@implementation NSCalendar: NSObject

+ (id)allocWithZone:(NSZonePtr)_zone {
    let host_object = Box::<NSCalendarHostObject>::default();
    env.objc.alloc_object(this, host_object, &mut env.mem)
}

- (id)initWithCalendarIdentifier:(id)identifier {
    retain(env, identifier);
    env.objc.borrow_mut::<NSCalendarHostObject>(this).identifier = identifier;
    this
}

- (id)calendarIdentifier {
    env.objc.borrow::<NSCalendarHostObject>(this).identifier
}

- (id)components:(NSUInteger)units fromDate:(id)date {
    let identifier = env.objc.borrow::<NSCalendarHostObject>(this).identifier;
    assert_eq!(to_rust_string(env, identifier), "gregorian");

    let absolute_time = env.objc.borrow::<NSDateHostObject>(date).time_interval;
    let unix_time = (absolute_time + SECS_FROM_UNIX_TO_APPLE_EPOCHS as f64).floor() as i32;
    let date_fields = timestamp_to_calendar_date(unix_time);

    let components: id = msg_class![env; NSDateComponents alloc];
    *env.objc.borrow_mut::<NSDateComponentsHostObject>(components) =
        NSDateComponentsHostObject::from_gregorian(units, date_fields);
    autorelease(env, components)
}

- (())dealloc {
    let identifier = std::mem::replace(&mut env.objc.borrow_mut::<NSCalendarHostObject>(this).identifier, nil);
    release(env, identifier);
    env.objc.dealloc_object(this, &mut env.mem)
}

@end

};
