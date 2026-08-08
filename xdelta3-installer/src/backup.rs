#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    #[test]
    fn flatten_rel_replaces_separators() {
        assert_eq!(
            flatten_rel(Path::new("Launcher3Configs/LauncherConfig.xml")),
            "Launcher3Configs__LauncherConfig.xml"
        );
        assert_eq!(
            flatten_rel(Path::new("game/Launcher3Configs/LauncherConfig.xml")),
            "game__Launcher3Configs__LauncherConfig.xml"
        );
    }

    #[test]
    fn manifest_json_round_trip() {
        let m = Manifest {
            installed: vec!["Launcher3Modules/xdelta.exe".to_string()],
            backups: vec![BackupEntry {
                source_rel: "Launcher3Modules/XDelta3WrapFactory.dll".to_string(),
                backup_rel: ".rxdelta3/backup/Launcher3Modules__XDelta3WrapFactory.dll".to_string(),
            }],
        };
        let v = json::parse(&m.to_json()).unwrap();
        assert_eq!(Manifest::from_json(&v).unwrap(), m);
    }

    #[test]
    fn backup_file_is_idempotent() {
        let td = TempDir::new();
        let target = td.path().join("launcher");
        std::fs::create_dir_all(target.join("Launcher3Modules")).unwrap();
        std::fs::write(
            target.join("Launcher3Modules/XDelta3WrapFactory.dll"),
            b"original",
        )
        .unwrap();
        assert!(backup_file(&target, "Launcher3Modules/XDelta3WrapFactory.dll").unwrap());
        assert!(!backup_file(&target, "Launcher3Modules/XDelta3WrapFactory.dll").unwrap());
        assert_eq!(
            std::fs::read(backup_path(
                &target,
                "Launcher3Modules/XDelta3WrapFactory.dll"
            ))
            .unwrap(),
            b"original"
        );
    }

    #[test]
    fn manifest_save_load() {
        let td = TempDir::new();
        let target = td.path().join("launcher");
        let m = Manifest {
            installed: vec![],
            backups: vec![],
        };
        save_manifest(&target, &m).unwrap();
        assert_eq!(load_manifest(&target).unwrap(), Some(m));

        let empty = TempDir::new();
        assert_eq!(load_manifest(empty.path()).unwrap(), None);
    }
}

use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use crate::json::{self, Json};

pub const BACKUP_DIR_REL: &str = ".rxdelta3/backup";
pub const MANIFEST_REL: &str = ".rxdelta3/manifest.json";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupEntry {
    /// Original file path relative to the target, forward-slash separated.
    pub source_rel: String,
    /// Backup path relative to the target, forward-slash separated.
    pub backup_rel: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    /// Files the installer created, forward-slash relative paths.
    pub installed: Vec<String>,
    pub backups: Vec<BackupEntry>,
}

impl Manifest {
    pub fn to_json(&self) -> String {
        let installed = Json::Array(
            self.installed
                .iter()
                .map(|s| Json::String(s.clone()))
                .collect(),
        );
        let backups = Json::Array(
            self.backups
                .iter()
                .map(|b| {
                    Json::Object(vec![
                        ("source".to_string(), Json::String(b.source_rel.clone())),
                        ("backup".to_string(), Json::String(b.backup_rel.clone())),
                    ])
                })
                .collect(),
        );
        json::to_string(&Json::Object(vec![
            ("installed".to_string(), installed),
            ("backups".to_string(), backups),
        ]))
    }

    pub fn from_json(v: &Json) -> Result<Self, String> {
        let installed = v
            .get("installed")
            .ok_or_else(|| "manifest missing \"installed\"".to_string())?
            .as_array()
            .ok_or_else(|| "\"installed\" must be an array".to_string())?
            .iter()
            .map(|x| {
                x.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| "\"installed\" entry not a string".to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;

        let backups = v
            .get("backups")
            .ok_or_else(|| "manifest missing \"backups\"".to_string())?
            .as_array()
            .ok_or_else(|| "\"backups\" must be an array".to_string())?
            .iter()
            .map(|x| {
                let obj = x
                    .as_object()
                    .ok_or_else(|| "\"backups\" entry not an object".to_string())?;
                let source_rel = obj_get(obj, "source")
                    .and_then(Json::as_str)
                    .ok_or_else(|| "backup entry missing \"source\"".to_string())?
                    .to_string();
                let backup_rel = obj_get(obj, "backup")
                    .and_then(Json::as_str)
                    .ok_or_else(|| "backup entry missing \"backup\"".to_string())?
                    .to_string();
                Ok(BackupEntry {
                    source_rel,
                    backup_rel,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;

        Ok(Manifest { installed, backups })
    }
}

fn obj_get<'a>(obj: &'a [(String, Json)], key: &str) -> Option<&'a Json> {
    obj.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

/// Flatten a relative path into a single filename: separators become `__`.
pub fn flatten_rel(rel: &Path) -> String {
    rel.components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s.to_string_lossy().replace(['/', '\\'], "__")),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("__")
}

/// Backup path for a config, as a forward-slash path relative to the target.
pub fn backup_rel(config_rel: &str) -> String {
    format!("{BACKUP_DIR_REL}/{}", flatten_rel(Path::new(config_rel)))
}

pub fn backup_dir(target: &Path) -> PathBuf {
    target.join(BACKUP_DIR_REL)
}

pub fn manifest_path(target: &Path) -> PathBuf {
    target.join(MANIFEST_REL)
}

pub fn backup_path(target: &Path, config_rel: &str) -> PathBuf {
    target.join(backup_rel(config_rel))
}

pub fn load_manifest(target: &Path) -> Result<Option<Manifest>, String> {
    let p = manifest_path(target);
    if !p.is_file() {
        return Ok(None);
    }
    let text = fs::read_to_string(&p).map_err(|e| format!("read {}: {e}", p.display()))?;
    let v = json::parse(&text).map_err(|e| format!("parse {}: {e}", p.display()))?;
    Manifest::from_json(&v)
        .map(Some)
        .map_err(|e| format!("manifest {}: {e}", p.display()))
}

pub fn save_manifest(target: &Path, m: &Manifest) -> io::Result<()> {
    let p = manifest_path(target);
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(p, m.to_json())
}

/// Copy a file to its backup location. Returns false when the backup already exists.
pub fn backup_file(target: &Path, source_rel: &str) -> io::Result<bool> {
    let src = target.join(source_rel);
    let dst = backup_path(target, source_rel);
    if dst.is_file() {
        return Ok(false);
    }
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(&src, &dst)?;
    Ok(true)
}
