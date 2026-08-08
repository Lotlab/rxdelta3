use std::fs;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Artifacts {
    pub dll: Vec<u8>,
    pub xdelta: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactKind {
    Dll,
    XDelta,
}

impl ArtifactKind {
    pub fn file_name(self) -> &'static str {
        match self {
            ArtifactKind::Dll => "xdelta3_wrap.dll",
            ArtifactKind::XDelta => "xdelta.exe",
        }
    }

    pub fn flag_name(self) -> &'static str {
        match self {
            ArtifactKind::Dll => "--dll",
            ArtifactKind::XDelta => "--xdelta",
        }
    }
}

pub fn resolve_artifacts(
    embedded_dll: &[u8],
    embedded_xdelta: &[u8],
    dll_flag: Option<&Path>,
    xdelta_flag: Option<&Path>,
    exe_dir: &Path,
) -> Result<Artifacts, String> {
    Ok(Artifacts {
        dll: resolve_one(ArtifactKind::Dll, embedded_dll, dll_flag, exe_dir)?,
        xdelta: resolve_one(ArtifactKind::XDelta, embedded_xdelta, xdelta_flag, exe_dir)?,
    })
}

fn resolve_one(
    kind: ArtifactKind,
    embedded: &[u8],
    flag: Option<&Path>,
    exe_dir: &Path,
) -> Result<Vec<u8>, String> {
    if let Some(p) = flag {
        return fs::read(p)
            .map_err(|e| format!("cannot read {} ({}): {e}", kind.file_name(), p.display()));
    }
    if !embedded.is_empty() {
        return Ok(embedded.to_vec());
    }
    let nearby = exe_dir.join(kind.file_name());
    if nearby.is_file() {
        return fs::read(&nearby).map_err(|e| format!("cannot read {}: {e}", nearby.display()));
    }
    Err(format!(
        "{} unavailable: not embedded, no {} flag, and {} not found next to this exe",
        kind.file_name(),
        kind.flag_name(),
        nearby.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;
    use std::fs;

    #[test]
    fn flag_wins_over_embedded_and_nearby() {
        let td = TempDir::new();
        let dir = td.path();
        let flag = dir.join("flag_dll.bin");
        fs::write(&flag, b"flag").unwrap();
        fs::write(dir.join("xdelta.exe"), b"nearby").unwrap();
        let art = resolve_artifacts(b"embedded", b"", Some(&flag), None, dir).unwrap();
        assert_eq!(art.dll, b"flag");
        assert_eq!(art.xdelta, b"nearby");
    }

    #[test]
    fn flag_used_when_not_embedded() {
        let td = TempDir::new();
        let dir = td.path();
        let flag = dir.join("flag_x.exe");
        fs::write(&flag, b"flagx").unwrap();
        fs::write(dir.join("xdelta3_wrap.dll"), b"nearbydll").unwrap();
        let art = resolve_artifacts(b"", b"", None, Some(&flag), dir).unwrap();
        assert_eq!(art.xdelta, b"flagx");
    }

    #[test]
    fn missing_errors_with_clear_message() {
        let td = TempDir::new();
        let err = resolve_artifacts(b"", b"", None, None, td.path()).unwrap_err();
        assert!(err.contains("xdelta3_wrap.dll"));
    }
}
