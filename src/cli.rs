//! Explorer paths are OsStrings and are never interpreted by a command shell.
use crate::{Result, ToolError};
use std::{ffi::OsString, path::PathBuf};
pub fn open_path(args: &[OsString]) -> Result<Option<PathBuf>> {
    match args {
        [] => Ok(None),
        [flag, path] if flag == "--open" && !path.is_empty() => Ok(Some(path.into())),
        [path] if !path.is_empty() && !path.to_string_lossy().starts_with("--") => {
            Ok(Some(path.into()))
        }
        _ => Err(ToolError::Invalid(
            "请使用：gloamere-image.exe --open \"图片路径\"，或传入单张图片路径".into(),
        )),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explorer_unicode_and_metacharacters_remain_a_single_path() {
        let path = OsString::from("C:\\中文 空格 & (素材)\\🌿.PNG");
        assert_eq!(
            open_path(&["--open".into(), path.clone()]).unwrap(),
            Some(PathBuf::from(&path))
        );
        assert_eq!(
            open_path(std::slice::from_ref(&path)).unwrap(),
            Some(PathBuf::from(path))
        );
        assert_eq!(
            open_path(&["--open".into(), "--image.png".into()]).unwrap(),
            Some("--image.png".into())
        );
    }
    #[test]
    fn missing_extra_and_unknown_options_are_rejected() {
        for args in [
            vec!["--open"],
            vec!["a.png", "b.png"],
            vec!["--open", "a.png", "b.png"],
            vec!["--unknown"],
            vec!["--open", ""],
        ] {
            assert!(open_path(&args.into_iter().map(OsString::from).collect::<Vec<_>>()).is_err());
        }
        assert_eq!(open_path(&[]).unwrap(), None);
    }
}
