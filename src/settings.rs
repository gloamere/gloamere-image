//! Preferences contain no image history and do not participate in exports.
use crate::{Result, ToolError};
use serde::{Deserialize, Serialize};
use std::{
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CanvasBackground {
    Dark,
    Light,
    CheckerDark,
    #[default]
    CheckerLight,
}
pub type Rgb = [u8; 3];
pub type CheckerColors = (Rgb, Rgb);
impl CanvasBackground {
    pub const ALL: [Self; 4] = [
        Self::CheckerLight,
        Self::CheckerDark,
        Self::Light,
        Self::Dark,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::Dark => "深色",
            Self::Light => "浅色",
            Self::CheckerDark => "深棋盘",
            Self::CheckerLight => "浅棋盘",
        }
    }
    pub fn colors(self) -> (Rgb, Option<CheckerColors>) {
        match self {
            Self::Dark => ([26, 30, 36], None),
            Self::Light => ([226, 230, 234], None),
            Self::CheckerDark => ([26, 30, 36], Some(([64; 3], [43; 3]))),
            Self::CheckerLight => ([226, 230, 234], Some(([249; 3], [235; 3]))),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Preferences {
    pub schema_version: u8,
    pub background: CanvasBackground,
    pub list_visible: bool,
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            schema_version: 1,
            background: CanvasBackground::default(),
            list_visible: false,
        }
    }
}
pub const MAX_SETTINGS_BYTES: u64 = 16 * 1024;
// Keep the legacy directory/schema so the gloamere-image rename preserves preferences.
pub fn user_settings_path() -> Option<PathBuf> {
    if let Some(directory) = std::env::var_os("LIGHT_IMAGE_SETTINGS_DIR") {
        return Some(PathBuf::from(directory).join("settings.json"));
    }
    #[cfg(windows)]
    {
        std::env::var_os("LOCALAPPDATA").map(|p| {
            PathBuf::from(p)
                .join("LightImageTool")
                .join("settings.json")
        })
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("HOME").map(|p| {
            PathBuf::from(p).join("Library/Application Support/LightImageTool/settings.json")
        })
    }
}
pub fn load(path: &Path) -> Result<Preferences> {
    use std::io::Read;
    let file = std::fs::File::open(path)?;
    let mut bytes = Vec::new();
    file.take(MAX_SETTINGS_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_SETTINGS_BYTES {
        return Err(ToolError::Invalid(
            "设置文件超过大小限制，使用默认设置".into(),
        ));
    }
    let settings: Preferences = serde_json::from_slice(&bytes)
        .map_err(|_| ToolError::Invalid("设置文件无效，使用默认设置".into()))?;
    if settings.schema_version != 1 {
        return Err(ToolError::Invalid("设置版本不支持，使用默认设置".into()));
    }
    Ok(settings)
}
pub fn save(path: &Path, settings: &Preferences) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| ToolError::Invalid("设置目录不存在".into()))?;
    std::fs::create_dir_all(parent)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(&mut temporary, settings)
        .map_err(|e| ToolError::Invalid(e.to_string()))?;
    temporary.write_all(b"\n")?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(path)
        .map_err(|e| ToolError::Io(e.error))?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn default_view_is_light_checker_with_collapsed_list() {
        assert_eq!(
            Preferences::default().background,
            CanvasBackground::CheckerLight
        );
        assert!(!Preferences::default().list_visible);
        assert_eq!(CanvasBackground::ALL.len(), 4);
    }
    #[test]
    fn preferences_round_trip_and_replace_without_history() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("中 文/settings.json");
        let mut preferences = Preferences::default();
        save(&path, &preferences).unwrap();
        preferences.background = CanvasBackground::Light;
        preferences.list_visible = false;
        save(&path, &preferences).unwrap();
        assert_eq!(load(&path).unwrap(), preferences);
        assert!(
            !std::fs::read_to_string(path)
                .unwrap()
                .contains("image_path")
        );
    }
    #[test]
    fn corrupt_oversized_future_and_unwritable_settings_are_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        for content in [
            b"bad".to_vec(),
            vec![b' '; MAX_SETTINGS_BYTES as usize + 1],
            br#"{"schema_version":2,"background":"dark","list_visible":true}"#.to_vec(),
        ] {
            std::fs::write(&path, content).unwrap();
            assert!(load(&path).is_err());
        }
        assert!(
            save(
                &dir.path().join("settings.json/child"),
                &Preferences::default()
            )
            .is_err()
        );
    }
}
