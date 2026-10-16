/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `NSDateComponents`.

use super::{NSInteger, NSUInteger};
use crate::libc::time::tm;
use crate::objc::{id, objc_classes, ClassExports, HostObject, NSZonePtr};

pub(super) const NS_UNDEFINED_DATE_COMPONENT: NSInteger = NSInteger::MAX;

#[derive(Debug)]
pub(super) struct NSDateComponentsHostObject {
    year: NSInteger,
    month: NSInteger,
    day: NSInteger,
    hour: NSInteger,
    minute: NSInteger,
    second: NSInteger,
}
impl HostObject for NSDateComponentsHostObject {}

impl Default for NSDateComponentsHostObject {
    fn default() -> Self {
        Self {
            year: NS_UNDEFINED_DATE_COMPONENT,
            month: NS_UNDEFINED_DATE_COMPONENT,
            day: NS_UNDEFINED_DATE_COMPONENT,
            hour: NS_UNDEFINED_DATE_COMPONENT,
            minute: NS_UNDEFINED_DATE_COMPONENT,
            second: NS_UNDEFINED_DATE_COMPONENT,
        }
    }
}

impl NSDateComponentsHostObject {
    pub(super) fn from_gregorian(units: NSUInteger, date: tm) -> Self {
        let mut components = Self::default();
        if units & (1 << 2) != 0 {
            components.year = date.tm_year + 1900;
        }
        if units & (1 << 3) != 0 {
            components.month = date.tm_mon + 1;
        }
        if units & (1 << 4) != 0 {
            components.day = date.tm_mday;
        }
        if units & (1 << 5) != 0 {
            components.hour = date.tm_hour;
        }
        if units & (1 << 6) != 0 {
            components.minute = date.tm_min;
        }
        if units & (1 << 7) != 0 {
            components.second = date.tm_sec;
        }
        components
    }
}

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

@implementation NSDateComponents: NSObject

+ (id)allocWithZone:(NSZonePtr)_zone {
    let host_object = Box::<NSDateComponentsHostObject>::default();
    env.objc.alloc_object(this, host_object, &mut env.mem)
}

- (NSInteger)year {
    env.objc.borrow::<NSDateComponentsHostObject>(this).year
}

- (NSInteger)month {
    env.objc.borrow::<NSDateComponentsHostObject>(this).month
}

- (NSInteger)day {
    env.objc.borrow::<NSDateComponentsHostObject>(this).day
}

- (NSInteger)hour {
    env.objc.borrow::<NSDateComponentsHostObject>(this).hour
}

- (NSInteger)minute {
    env.objc.borrow::<NSDateComponentsHostObject>(this).minute
}

- (NSInteger)second {
    env.objc.borrow::<NSDateComponentsHostObject>(this).second
}

@end

};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::libc::time::timestamp_to_calendar_date;

    #[test]
    fn requested_gregorian_components() {
        // 2024-02-29 12:34:56 UTC
        let date = timestamp_to_calendar_date(1_709_210_096);
        let components = NSDateComponentsHostObject::from_gregorian(0xfc, date);
        assert_eq!(
            (components.year, components.month, components.day),
            (2024, 2, 29)
        );
        assert_eq!(
            (components.hour, components.minute, components.second),
            (12, 34, 56)
        );
    }

    #[test]
    fn unrequested_components_are_undefined() {
        let date = timestamp_to_calendar_date(1_709_210_096);
        let components = NSDateComponentsHostObject::from_gregorian(1 << 2, date);
        assert_eq!(components.year, 2024);
        assert_eq!(components.month, NS_UNDEFINED_DATE_COMPONENT);
        assert_eq!(components.day, NS_UNDEFINED_DATE_COMPONENT);
    }
}
