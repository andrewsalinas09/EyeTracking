//! UI preferences are separate from calibration and private learning journals.
use serde::{Deserialize, Serialize};
pub const DEFAULT_REARM_MS: u32 = 300;
pub const MIN_REARM_MS: u32 = 50;
pub const MAX_REARM_MS: u32 = 2000;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    pub enabled: bool,
    pub dot: bool,
    pub scroll: bool,
    pub learning: bool,
    pub mouse_rearm_ms: u32,
    pub trackpad_rearm_ms: u32,
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            enabled: true,
            dot: true,
            scroll: true,
            learning: true,
            mouse_rearm_ms: DEFAULT_REARM_MS,
            trackpad_rearm_ms: 0,
        }
    }
}
impl Preferences {
    pub fn load() -> Self {
        let mut p: Self = std::fs::read("recordings/preferences.json")
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        p.mouse_rearm_ms = p.mouse_rearm_ms.clamp(MIN_REARM_MS, MAX_REARM_MS);
        p.trackpad_rearm_ms = p.trackpad_rearm_ms.min(MAX_REARM_MS);
        p
    }
    pub fn save(self) -> Result<(), String> {
        std::fs::create_dir_all("recordings").map_err(|e| e.to_string())?;
        let bytes = serde_json::to_vec_pretty(&self).map_err(|e| e.to_string())?;
        std::fs::write("recordings/preferences.tmp", bytes).map_err(|e| e.to_string())?;
        std::fs::rename("recordings/preferences.tmp", "recordings/preferences.json")
            .map_err(|e| e.to_string())
    }
}

/// A shortcut or Explorer launch must find the same data as `cargo run`.
/// Development builds keep existing recordings; a standalone install uses LocalAppData.
pub fn set_data_directory() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let root = exe
        .ancestors()
        .skip(1)
        .find(|p| p.join("Cargo.toml").is_file() && p.join("src/tobii.rs").is_file())
        .map(|p| p.to_path_buf())
        .or_else(|| {
            std::env::var_os("LOCALAPPDATA")
                .map(|p| std::path::PathBuf::from(p).join("EyeTracking"))
        })
        .ok_or("Could not locate EyeTracking's data directory")?;
    std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    std::env::set_current_dir(root).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_new_preferences_keep_enabled_defaults() {
        let p: Preferences = serde_json::from_str(r#"{"dot":false}"#).unwrap();
        assert!(!p.dot);
        assert!(p.enabled && p.scroll && p.learning);
        assert_eq!(p.mouse_rearm_ms, DEFAULT_REARM_MS);
        assert_eq!(p.trackpad_rearm_ms, 0);
        assert_eq!(
            p,
            serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap()
        );
    }
}
