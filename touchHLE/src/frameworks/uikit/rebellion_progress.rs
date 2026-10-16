// This game keeps its defaced-poster IDs only in a C++ vector. Its SQLite
// mission_state is a quest stage, and mission_data is not populated for them.
// Preserve that vector in the application's own Documents directory so both
// the objective counter and ScenePoster's appearance survive a restart.
use crate::mem::{ConstPtr, Ptr};
use crate::{msg, Environment};
use std::io::Cursor;

const MAGIC: &[u8; 4] = b"8BP1";
const MAX_POSTERS: usize = 24;

/// In-memory progress can be newer than the file after a failed write.
/// Retain it across mission object replacement until persistence succeeds.
#[derive(Clone)]
pub(super) struct PosterProgress {
    pub mission: u32,
    pub ids: Vec<i32>,
    pending_write: bool,
}
impl PosterProgress {
    pub fn saved(mission: u32, ids: Vec<i32>) -> Self {
        Self {
            mission,
            ids,
            pending_write: false,
        }
    }

    fn persist_with(&mut self, write: impl FnOnce(&[i32]) -> bool) -> bool {
        if !self.pending_write {
            return true;
        }
        if write(&self.ids) {
            self.pending_write = false;
            true
        } else {
            false
        }
    }
}

pub(super) fn write_atomically(
    fs: &mut crate::fs::Fs,
    path: &crate::fs::GuestPath,
    bytes: &[u8],
) -> bool {
    let temporary_name = format!("{}.tmp", path.as_str());
    let temporary = crate::fs::GuestPath::new(&temporary_name);
    fs.write(temporary, bytes).is_ok() && fs.rename(temporary, path).is_ok()
}

// The game writes 1 when a tutorial is first displayed and 2 only after its
// guided completion callback. At the next launch its loadState turns every 1
// back into 0, causing the same popup to return. For this offline campaign,
// a displayed tutorial should be remembered across sessions.
fn completed_tutorial_plist(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut value = plist::Value::from_reader(Cursor::new(bytes)).ok()?;
    let states = value.as_array_mut()?;
    if states.len() != 45
        || !states
            .iter()
            .all(|v| matches!(v.as_signed_integer(), Some(0..=2)))
    {
        return None;
    }
    let mut changed = false;
    for state in states {
        if state.as_signed_integer() == Some(1) {
            *state = plist::Value::Integer(2.into());
            changed = true;
        }
    }
    if !changed {
        return None;
    }
    let mut out = Vec::new();
    value.to_writer_binary(&mut out).ok()?;
    Some(out)
}

pub(crate) fn finish_previously_shown_tutorials(fs: &mut crate::fs::Fs) {
    let path = fs.home_directory().join("Documents/Player_Tutorial");
    if let Ok(bytes) = fs.read(&path) {
        if let Some(completed) = completed_tutorial_plist(&bytes) {
            if write_atomically(fs, &path, &completed) {
                log!("Preserved previously displayed 8-Bit Rebellion tutorials");
            }
        }
    }
}

fn decode(bytes: &[u8]) -> Option<Vec<i32>> {
    if bytes.len() < 5 || &bytes[..4] != MAGIC {
        return None;
    }
    let count = bytes[4] as usize;
    if count > MAX_POSTERS || bytes.len() != 5 + 4 * count {
        return None;
    }
    let mut ids = Vec::with_capacity(count);
    for chunk in bytes[5..].chunks_exact(4) {
        let id = i32::from_le_bytes(chunk.try_into().unwrap());
        if id < 0 || ids.contains(&id) {
            return None;
        }
        ids.push(id);
    }
    Some(ids)
}

pub(super) fn encode(ids: &[i32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(5 + 4 * ids.len());
    bytes.extend_from_slice(MAGIC);
    bytes.push(ids.len() as u8);
    for id in ids {
        bytes.extend_from_slice(&id.to_le_bytes());
    }
    bytes
}

fn merge_poster_ids(saved: &mut Vec<i32>, current: &[i32]) -> Vec<i32> {
    for id in current {
        if !saved.contains(id) {
            saved.push(*id);
        }
    }
    saved
        .iter()
        .copied()
        .filter(|id| !current.contains(id))
        .collect()
}

pub(super) fn poster_ids(env: &Environment, mission: crate::objc::id) -> Option<Vec<i32>> {
    let ivar = env
        .objc
        .object_lookup_ivar(&env.mem, mission, &"defacedPosterList".to_owned())?;
    let vector: u32 = env.mem.read(ivar);
    if vector == 0
        || env
            .mem
            .get_bytes_fallible(Ptr::from_bits(vector), 12)
            .is_none()
    {
        return None;
    }
    let begin: u32 = env.mem.read(ConstPtr::from_bits(vector));
    let end: u32 = env.mem.read(ConstPtr::from_bits(vector + 4));
    let capacity: u32 = env.mem.read(ConstPtr::from_bits(vector + 8));
    if begin > end || end > capacity || (end - begin) % 4 != 0 {
        return None;
    }
    let count = ((end - begin) / 4) as usize;
    if count > MAX_POSTERS
        || (count > 0
            && env
                .mem
                .get_bytes_fallible(Ptr::from_bits(begin), end - begin)
                .is_none())
    {
        return None;
    }
    let ids: Vec<i32> = (0..count)
        .map(|i| env.mem.read(ConstPtr::from_bits(begin + 4 * i as u32)))
        .collect();
    (ids.iter().all(|&id| id >= 0) && ids.iter().enumerate().all(|(i, id)| !ids[..i].contains(id)))
        .then_some(ids)
}

pub(super) fn sync_posters(env: &mut Environment) {
    if env.bundle.bundle_identifier() != "com.alife.linkinpark"
        || env.bundle.bundle_version() != "1.4.5"
    {
        return;
    }
    let Some(class) = env
        .objc
        .all_classes()
        .find(|(name, _)| name.as_str() == "LPMission1")
        .map(|(_, &class)| class)
    else {
        return;
    };
    let instances = env.objc.instances_of_class(class, &env.mem);
    let [mission] = instances.as_slice() else {
        return; // Keep any unsaved IDs while the mission is absent.
    };
    let mission = *mission;
    let Some(ids) = poster_ids(env, mission) else {
        return;
    };
    let path = env
        .fs
        .home_directory()
        .join("Documents/8beet-poster-progress-v1");
    let cached = env.framework_state.uikit.rebellion_posters.as_ref();
    let first_seen = cached.map(|progress| progress.mission) != Some(mission.to_bits());
    let was_pending = cached.is_some_and(|progress| progress.pending_write);
    let mut saved = if first_seen {
        let mut saved = env
            .fs
            .read(&path)
            .ok()
            .and_then(|bytes| decode(&bytes))
            .unwrap_or_default();
        if let Some(progress) = cached.filter(|progress| progress.pending_write) {
            merge_poster_ids(&mut saved, &progress.ids);
        }
        saved
    } else {
        cached.unwrap().ids.clone()
    };
    let original_count = saved.len();
    let missing = merge_poster_ids(&mut saved, &ids);
    if saved.len() > MAX_POSTERS {
        log!(
            "Ignoring invalid 8-Bit Rebellion poster count {}",
            saved.len()
        );
        return;
    }
    if !missing.is_empty() {
        for id in missing {
            let _: () = msg![env; mission defacePoster:id];
        }
        log!("Restored {} 8-Bit Rebellion posters", saved.len());
    }
    // A new mission object may reuse the same guest pointer. Its empty vector
    // is temporary, not a request to erase progress. Poster collection only
    // grows within this quest; save the union, never a smaller list.
    let pending_write = was_pending
        || saved.len() > original_count
        || (first_seen && !saved.is_empty() && !env.fs.exists(&path));
    let mut progress = PosterProgress::saved(mission.to_bits(), saved);
    progress.pending_write = pending_write;
    if progress.pending_write {
        if progress.persist_with(|ids| write_atomically(&mut env.fs, &path, &encode(ids))) {
            log!("Saved {} 8-Bit Rebellion posters", progress.ids.len());
        } else if !was_pending {
            log!("Could not save 8-Bit Rebellion posters; retaining progress for retry");
        }
    }
    env.framework_state.uikit.rebellion_posters = Some(progress);
}

#[cfg(test)]
mod tests {
    use super::{completed_tutorial_plist, decode, encode, merge_poster_ids};

    #[test]
    fn poster_progress_round_trip_and_rejects_corruption() {
        let ids = [3, 8, 12];
        assert_eq!(decode(&encode(&ids)), Some(ids.to_vec()));
        assert_eq!(decode(&encode(&[3, 3])), None);
        assert_eq!(decode(&encode(&ids)[..6]), None);
    }

    #[test]
    fn all_twenty_four_mission_one_posters_can_be_saved() {
        let ids: Vec<i32> = (0..24).collect();
        assert_eq!(decode(&encode(&ids)), Some(ids));
        assert_eq!(decode(&encode(&(0..25).collect::<Vec<_>>())), None);
    }

    #[test]
    fn an_empty_new_mission_does_not_erase_collected_posters() {
        let mut saved = vec![3, 8];
        assert_eq!(merge_poster_ids(&mut saved, &[]), vec![3, 8]);
        assert_eq!(saved, vec![3, 8]);
        assert_eq!(merge_poster_ids(&mut saved, &[3, 8, 12]), Vec::<i32>::new());
        assert_eq!(saved, vec![3, 8, 12]);
    }

    #[test]
    fn failed_poster_write_is_retried_without_new_collection() {
        let mut progress = super::PosterProgress {
            mission: 123,
            ids: vec![3, 8, 12],
            pending_write: true,
        };
        assert!(!progress.persist_with(|ids| {
            assert_eq!(ids, &[3, 8, 12]);
            false
        }));
        assert!(progress.pending_write);
        // The next sync sees the same IDs, but must still write them.
        assert!(merge_poster_ids(&mut progress.ids, &[3, 8, 12]).is_empty());
        assert!(progress.persist_with(|ids| {
            assert_eq!(super::decode(&super::encode(ids)), Some(vec![3, 8, 12]));
            true
        }));
        assert!(!progress.pending_write);
        assert!(progress.persist_with(|_| panic!("Clean progress must not be rewritten")));
    }

    #[test]
    fn displayed_tutorials_are_marked_complete_without_touching_other_states() {
        let mut states = vec![plist::Value::Integer(0.into()); 45];
        states[1] = plist::Value::Integer(1.into());
        states[2] = plist::Value::Integer(2.into());
        let mut bytes = Vec::new();
        plist::Value::Array(states)
            .to_writer_binary(&mut bytes)
            .unwrap();
        let updated = completed_tutorial_plist(&bytes).unwrap();
        let parsed = plist::Value::from_reader(std::io::Cursor::new(updated)).unwrap();
        let values = parsed.as_array().unwrap();
        assert_eq!(values[0].as_signed_integer(), Some(0));
        assert_eq!(values[1].as_signed_integer(), Some(2));
        assert_eq!(values[2].as_signed_integer(), Some(2));
    }
}
