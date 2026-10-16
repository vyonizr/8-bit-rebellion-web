/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! Explicitly compiled-in local test tools for the inspected iPhone 1.4.5.
use crate::mem::{ConstPtr, Ptr};
use crate::objc::{id, nil};
use crate::{msg, msg_class, Environment};

#[derive(Default)]
pub(super) struct State {
    coins: Option<i32>,
    posters: Option<(&'static str, Vec<i32>, i32)>,
    next_achievement: i32,
}

fn instances(env: &Environment, name: &str) -> Vec<id> {
    env.objc
        .all_classes()
        .find(|(n, _)| n.as_str() == name)
        .map(|(_, &class)| env.objc.instances_of_class(class, &env.mem))
        .unwrap_or_default()
}

pub(super) fn apply(env: &mut Environment, action: u8) {
    if env.bundle.bundle_identifier() != "com.alife.linkinpark"
        || env.bundle.bundle_version() != "1.4.5"
        || std::env::var_os("BEBELLION_TEST_DATA_DIR").is_none()
    {
        log!("Test tools require iPhone 1.4.5 and an isolated test data directory");
        return;
    }
    let monitors = instances(env, "PlayerMonitorComponent");
    let [monitor] = monitors.as_slice() else {
        log!("Test tools: enter the playable world first");
        return;
    };
    let monitor = *monitor;
    let account: id = msg_class![env; AccountHandler getHandler];
    let online: bool = msg![env; account isOnlineMode];
    if online {
        log!("Test tools are restricted to the offline campaign");
        return;
    }
    let pool: id = msg_class![env; NSAutoreleasePool new];
    match action {
        1 => coins(env, monitor),
        2 => posters(env),
        3 => achievement(env, monitor),
        _ => (),
    }
    crate::objc::release(env, pool);
}

fn coins(env: &mut Environment, monitor: id) {
    let connector: id = msg_class![env; PlayerConnector getConnector];
    let profile: id = msg![env; connector getSelfProfile];
    if profile == nil {
        return;
    }
    let current: i32 = msg![env; profile credits];
    let original = env.framework_state.uikit.rebellion_test_tools.coins;
    let target = original.unwrap_or(9999);
    if original.is_none() {
        env.framework_state.uikit.rebellion_test_tools.coins = Some(current);
    }
    // Use the same absolute update and profile refresh as addCurrency:.
    let _: () = msg![env; connector playerCreditUpdate:target];
    let profile: id = msg![env; connector getSelfProfile];
    let _: () = msg![env; monitor setWithPlayerProfile:profile];
    for ui in instances(env, "SceneUIComponent") {
        let _: () = msg![env; ui updateCoin];
    }
    let actual: i32 = msg![env; profile credits];
    if actual == target && original.is_some() {
        env.framework_state.uikit.rebellion_test_tools.coins = None;
    }
    log!(
        "Test coins: {} -> {} (requested {}; undo baseline {:?})",
        current,
        actual,
        target,
        env.framework_state.uikit.rebellion_test_tools.coins
    );
}

fn posters(env: &mut Environment) {
    let snapshot = env
        .framework_state
        .uikit
        .rebellion_test_tools
        .posters
        .clone();
    let mut active = None;
    for (name, required_state) in [("LPMission1", 7), ("LPMission5", 1)] {
        if snapshot
            .as_ref()
            .is_some_and(|(saved_name, _, _)| *saved_name != name)
        {
            continue;
        }
        let missions = instances(env, name);
        if let [mission] = missions.as_slice() {
            let mission = *mission;
            let quest: id = msg![env; mission mission];
            let state: i32 = msg![env; quest missionState];
            if snapshot.is_some() || state == required_state {
                active = Some((name, mission, state));
                break;
            }
        }
    }
    let Some((name, mission, state)) = active else {
        log!("Test posters: enter an active poster quest (Mission 1 state 7 or Mission 5 state 1)");
        return;
    };
    let Some(current) = super::rebellion_progress::poster_ids(env, mission) else {
        return;
    };

    if let Some((_, ids, old_state)) = env
        .framework_state
        .uikit
        .rebellion_test_tools
        .posters
        .clone()
    {
        // The vector still owns its allocation. Shrink/copy elements without
        // replacing its storage or bypassing the guest allocator.
        let field = env
            .objc
            .object_lookup_ivar(&env.mem, mission, &"defacedPosterList".to_owned())
            .unwrap();
        let vector: u32 = env.mem.read(field);
        let begin: u32 = env.mem.read(ConstPtr::from_bits(vector));
        let capacity: u32 = env.mem.read(ConstPtr::from_bits(vector + 8));
        if capacity < begin || capacity - begin < ids.len() as u32 * 4 {
            return;
        }
        if name == "LPMission1" {
            let path = env
                .fs
                .home_directory()
                .join("Documents/8beet-poster-progress-v1");
            if !super::rebellion_progress::write_atomically(
                &mut env.fs,
                &path,
                &super::rebellion_progress::encode(&ids),
            ) {
                return;
            }
        }
        for (i, value) in ids.iter().enumerate() {
            env.mem.write(Ptr::from_bits(begin + i as u32 * 4), *value);
        }
        env.mem
            .write(Ptr::from_bits(vector + 4), begin + ids.len() as u32 * 4);
        if name == "LPMission1" {
            env.framework_state.uikit.rebellion_posters = Some(
                super::rebellion_progress::PosterProgress::saved(mission.to_bits(), ids.clone()),
            );
        }
        let _: () = msg![env; mission setMissionState:old_state];
        refresh_poster_objective(env, name);
        env.framework_state.uikit.rebellion_test_tools.posters = None;
        log!(
            "Test posters: restored {} IDs and quest state {}. Change district to refresh artwork.",
            ids.len(),
            old_state
        );
    } else {
        env.framework_state.uikit.rebellion_test_tools.posters = Some((name, current, state));
        // Constructor IDs and active quest states inspected in IPA 1.4.5.
        let range = if name == "LPMission1" {
            0i32..24
        } else {
            24i32..44
        };
        for poster in range {
            let _: () = msg![env; mission defacePoster:poster];
        }
        if name == "LPMission1" {
            super::rebellion_progress::sync_posters(env);
        }
        let count: i32 = msg![env; mission defacedPosterCount];
        log!(
            "Test posters: {} collected {}. Use the same menu action to undo.",
            name,
            count
        );
    }
}

fn refresh_poster_objective(env: &mut Environment, name: &str) {
    for component in instances(env, "MissionProgressComponent") {
        if name == "LPMission1" {
            let _: () = msg![env; component updateMission1Objective];
        } else {
            let _: () = msg![env; component updateMission5Objective];
        }
    }
}

fn achievement(env: &mut Environment, monitor: id) {
    let index = env
        .framework_state
        .uikit
        .rebellion_test_tools
        .next_achievement;
    if index >= 16 {
        log!("Test achievements: all 16 IDs visited; restart test session to repeat");
        return;
    }
    let achievement_id = 22000 + index;
    let existed: bool = msg![env; monitor isAchievementExistWithAchievementID:achievement_id];
    log!(
        "Test achievement {}: beginning normal activation, already present={}",
        achievement_id,
        existed
    );
    if !existed {
        // Public game method applies thresholds and normal popup/persistence.
        // One achievement per action keeps any crash attributable to one ID.
        let _: () =
            msg![env; monitor addAchievementWithID:achievement_id WithCurrentNumber:10000i32];
    }
    let activated: bool = msg![env; monitor isAchievementExistWithAchievementID:achievement_id];
    log!(
        "Test achievement {}: activation returned, active={}",
        achievement_id,
        activated
    );
    if activated {
        env.framework_state
            .uikit
            .rebellion_test_tools
            .next_achievement += 1;
    } else {
        log!("Close game dialogs and retry this achievement");
    }
}
