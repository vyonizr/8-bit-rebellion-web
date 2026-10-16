/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! Host mute control for all game audio, independent of guest preferences.
use std::path::PathBuf;
fn path() -> PathBuf {
    #[cfg(feature = "rebellion-test-tools")]
    if let Some(directory) = std::env::var_os("BEBELLION_DISPLAY_SETTINGS_DIR") {
        return PathBuf::from(directory).join("8beet-sound-settings-v1");
    }
    crate::paths::user_data_base_path().join("8beet-sound-settings-v1")
}
fn decode(text: &str) -> Option<bool> {
    match text {
        "version=1\nmuted=0\n" => Some(false),
        "version=1\nmuted=1\n" => Some(true),
        _ => None,
    }
}
pub fn initialize(env: &mut crate::Environment) {
    if !env.options.display_service {
        return;
    }
    let saved = std::fs::read_to_string(path())
        .ok()
        .and_then(|text| decode(&text))
        .unwrap_or(false);
    let force_quiet = std::env::var("BEBELLION_MUTE").is_ok_and(|value| value == "1");
    env.openal_manager.set_muted(saved || force_quiet);
    env.options.sound_muted = env.openal_manager.is_muted();
    log!(
        "Sound at startup: {}",
        if env.openal_manager.is_muted() {
            "muted"
        } else {
            "on"
        }
    );
}
/// Apply the pending host audio choice after the settings modal closes.
pub fn set_muted(env: &mut crate::Environment, muted: bool) {
    if !env.options.display_service {
        return;
    }
    env.openal_manager.set_muted(muted);
    env.options.sound_muted = muted;
    crate::frameworks::media_player::set_muted(env, muted);
    let path = path();
    let temporary = path.with_extension("tmp");
    let result = (|| -> std::io::Result<()> {
        use std::io::Write;
        let mut file = std::fs::File::create(&temporary)?;
        write!(file, "version=1\nmuted={}\n", u8::from(muted))?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(temporary, path)
    })();
    log!("Sound: {}", if muted { "muted" } else { "on" });
    if let Err(error) = result {
        log!("Sound changed but could not be saved: {}", error);
    }
}
#[cfg(test)]
mod tests {
    use super::decode;
    #[test]
    fn sound_preferences_require_a_valid_version_and_boolean() {
        assert_eq!(decode("version=1\nmuted=1\n"), Some(true));
        assert_eq!(decode("version=1\nmuted=0\n"), Some(false));
        for text in [
            "version=2\nmuted=1\n",
            "version=1\nmuted=2\n",
            "muted=1\n",
            "version=1\nmuted=1\nextra=0\n",
        ] {
            assert_eq!(decode(text), None);
        }
    }
}
