//! Preferences contain no image history and do not participate in exports.
use crate::{Result, ToolError};
use serde::{Deserialize, Serialize};
use std::{
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Appearance {
    #[default]
    System,
    Light,
    Dark,
}
impl Appearance {
    pub const ALL: [Self; 3] = [Self::System, Self::Light, Self::Dark];

    pub fn label(self) -> &'static str {
        match self {
            Self::System => "跟随系统",
            Self::Light => "浅色",
            Self::Dark => "深色",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CanvasBackground {
    #[default]
    Automatic,
    Dark,
    Light,
    CheckerDark,
    CheckerLight,
}
pub type Rgb = [u8; 3];
pub type CheckerColors = (Rgb, Rgb);
impl CanvasBackground {
    pub const ALL: [Self; 5] = [
        Self::Automatic,
        Self::CheckerLight,
        Self::CheckerDark,
        Self::Light,
        Self::Dark,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::Automatic => "随外观",
            Self::Dark => "深色",
            Self::Light => "浅色",
            Self::CheckerDark => "深棋盘",
            Self::CheckerLight => "浅棋盘",
        }
    }
    pub fn colors(self) -> (Rgb, Option<CheckerColors>) {
        match self {
            // Theme-aware callers resolve Automatic before drawing; this is a
            // neutral light fallback for callers without an appearance context.
            Self::Automatic => ([0xE8; 3], None),
            Self::Dark => ([26, 30, 36], None),
            Self::Light => ([226, 230, 234], None),
            Self::CheckerDark => ([26, 30, 36], Some(([64; 3], [43; 3]))),
            Self::CheckerLight => ([241, 243, 242], Some(([241, 243, 242], [235, 239, 237]))),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Preferences {
    pub schema_version: u8,
    pub background: CanvasBackground,
    pub list_visible: bool,
    // Older v1 files omit appearance but still retain their saved canvas/list.
    #[serde(default)]
    pub appearance: Appearance,
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            schema_version: 1,
            background: CanvasBackground::default(),
            list_visible: false,
            appearance: Appearance::default(),
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
            "设置文件超过大小限制，暂用默认设置；原文件保留，偏好更改不会覆盖它".into(),
        ));
    }
    let settings: Preferences = serde_json::from_slice(&bytes).map_err(|_| {
        ToolError::Invalid(
            "设置文件无效或含未知字段，暂用默认设置；原文件保留，偏好更改不会覆盖它".into(),
        )
    })?;
    if settings.schema_version != 1 {
        return Err(ToolError::Invalid(
            "设置版本不支持，暂用默认设置；原文件保留，偏好更改不会覆盖它".into(),
        ));
    }
    Ok(settings)
}
pub fn save(path: &Path, settings: &Preferences) -> Result<()> {
    if settings.schema_version != 1 {
        return Err(ToolError::Invalid("不能写入不支持的设置版本".into()));
    }
    // A failed load does not authorize replacing the original on the next UI
    // toggle. Refuse rather than silently destroying future/unknown preferences
    // or a corrupt file the user may want to recover. No schema/path migration.
    match load(path) {
        Ok(_) => {}
        Err(ToolError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(ToolError::Invalid(format!(
                "本次偏好未保存，现有设置文件已保留：{error}"
            )));
        }
    }
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
    fn default_view_follows_appearance_with_collapsed_list() {
        assert_eq!(
            Preferences::default().background,
            CanvasBackground::Automatic
        );
        assert_eq!(Preferences::default().appearance, Appearance::System);
        assert!(!Preferences::default().list_visible);
        assert_eq!(CanvasBackground::ALL.len(), 5);
    }
    #[test]
    fn approved_light_checker_colors_are_exact() {
        assert_eq!(
            CanvasBackground::CheckerLight.colors(),
            (
                [0xF1, 0xF3, 0xF2],
                Some(([0xF1, 0xF3, 0xF2], [0xEB, 0xEF, 0xED]))
            )
        );
    }
    #[test]
    fn failed_load_never_allows_next_toggle_to_overwrite_original() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        for original in [
            b"partially written settings".to_vec(),
            vec![b' '; MAX_SETTINGS_BYTES as usize + 1],
            br#"{"schema_version":2,"background":"dark","list_visible":true}"#.to_vec(),
            br#"{"schema_version":1,"background":"dark","list_visible":true,"future_preference":42}"#.to_vec(),
            br#"{"schema_version":1,"background":"new_background","list_visible":true}"#.to_vec(),
            br#"{"schema_version":1,"background":"dark","list_visible":true,"appearance":"new_appearance"}"#.to_vec(),
            br#"{"schema_version":1,"background":"dark","list_visible":true,"appearance":null}"#.to_vec(),
            br#"{"schema_version":1,"background":"dark","list_visible":true,"appearance":"light","appearance":"dark"}"#.to_vec(),
        ] {
            std::fs::write(&path, &original).unwrap();
            assert!(load(&path).is_err());
            let preferences = Preferences { background: CanvasBackground::Dark, ..Preferences::default() };
            let error = save(&path, &preferences).unwrap_err().to_string();
            assert!(error.contains("本次偏好未保存"), "{error}");
            assert!(error.contains("已保留"), "{error}");
            assert_eq!(std::fs::read(&path).unwrap(), original);
            assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
        }
    }
    #[test]
    fn unsupported_outgoing_schema_is_refused_before_creating_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let preferences = Preferences {
            schema_version: 2,
            ..Preferences::default()
        };
        assert!(save(&path, &preferences).is_err());
        assert!(!path.exists());
    }
    #[test]
    fn existing_v1_preferences_preserve_every_supported_field() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(
            &path,
            br#"{"schema_version":1,"background":"checker_dark","list_visible":true}"#,
        )
        .unwrap();
        let mut preferences = load(&path).unwrap();
        assert_eq!(preferences.background, CanvasBackground::CheckerDark);
        assert!(preferences.list_visible);
        assert_eq!(preferences.appearance, Appearance::System);
        preferences.appearance = Appearance::Dark;
        save(&path, &preferences).unwrap();
        assert_eq!(load(&path).unwrap(), preferences);
        assert_eq!(preferences.schema_version, 1);
        assert_eq!(preferences.background, CanvasBackground::CheckerDark);
        assert!(preferences.list_visible);
    }
    #[test]
    fn loading_legacy_canvas_choices_never_rewrites_or_replaces_them() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        for background in [
            CanvasBackground::Dark,
            CanvasBackground::Light,
            CanvasBackground::CheckerDark,
            CanvasBackground::CheckerLight,
        ] {
            let original = serde_json::to_vec(&serde_json::json!({
                "schema_version": 1,
                "background": background,
                "list_visible": true,
            }))
            .unwrap();
            std::fs::write(&path, &original).unwrap();
            let preferences = load(&path).unwrap();
            assert_eq!(preferences.background, background);
            assert_eq!(preferences.appearance, Appearance::System);
            assert!(preferences.list_visible);
            assert_eq!(std::fs::read(&path).unwrap(), original);
        }
    }
    #[test]
    fn extended_v1_preferences_are_rejected_by_the_legacy_shape() {
        #[derive(Debug, Deserialize)]
        #[serde(deny_unknown_fields)]
        struct LegacyPreferences {
            schema_version: u8,
            background: CanvasBackground,
            list_visible: bool,
        }

        let old = br#"{"schema_version":1,"background":"checker_dark","list_visible":true}"#;
        let legacy: LegacyPreferences = serde_json::from_slice(old).unwrap();
        assert_eq!(legacy.schema_version, 1);
        assert_eq!(legacy.background, CanvasBackground::CheckerDark);
        assert!(legacy.list_visible);

        let preferences = Preferences {
            background: CanvasBackground::CheckerDark,
            appearance: Appearance::Dark,
            list_visible: true,
            ..Preferences::default()
        };
        let extended = serde_json::to_vec(&preferences).unwrap();
        let error = serde_json::from_slice::<LegacyPreferences>(&extended)
            .unwrap_err()
            .to_string();
        assert!(error.contains("unknown field `appearance`"), "{error}");
    }
    #[test]
    fn preferences_round_trip_and_replace_without_history() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("中 文/settings.json");
        let mut preferences = Preferences::default();
        save(&path, &preferences).unwrap();
        for appearance in Appearance::ALL {
            for background in CanvasBackground::ALL {
                preferences.appearance = appearance;
                preferences.background = background;
                preferences.list_visible = !preferences.list_visible;
                save(&path, &preferences).unwrap();
                assert_eq!(load(&path).unwrap(), preferences);
            }
        }
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
