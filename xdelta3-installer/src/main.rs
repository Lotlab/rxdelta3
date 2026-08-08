//! xdelta3-installer: install / rollback the xdelta3-wrap launcher DLL.

#![windows_subsystem = "windows"]

mod artifact;
mod backup;
mod gui;
mod json;
mod win32;

mod embedded {
    include!(concat!(env!("OUT_DIR"), "/embedded.rs"));
}

use std::path::{Path, PathBuf};
use std::process::ExitCode;

const XDELTA_SRC_REL: &str = "Launcher3Modules/XDelta3WrapFactory.dll";
const XDELTA_DEST_REL: &str = "Launcher3Modules/xdelta.exe";
const LAUNCHER_CONFIG_REL: &str = "Launcher3Configs/LauncherConfig.xml";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        // Double-click: GUI toggle mode. Target defaults to this exe's dir.
        let exe_dir = std::env::current_exe()
            .map_err(|e| format!("cannot resolve this exe's path: {e}"))
            .unwrap_or_default()
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_default();
        let target = exe_dir.clone();
        match gui::run_gui(&target, &exe_dir) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::FAILURE
            }
        }
    } else {
        // CLI mode: show output when launched from a console.
        win32::attach_console();
        match run(&args) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::FAILURE
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Command {
    #[default]
    Install,
    Rollback,
}

#[derive(Debug, Default)]
struct Options {
    command: Command,
    target: Option<PathBuf>,
    dll: Option<PathBuf>,
    xdelta: Option<PathBuf>,
}

fn run(args: &[String]) -> Result<(), String> {
    let opts = parse_args(args)?;
    let exe_dir = std::env::current_exe()
        .map_err(|e| format!("cannot resolve this exe's path: {e}"))?
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_default();
    let target = opts.target.clone().unwrap_or(exe_dir.clone());
    match opts.command {
        Command::Install => install(&target, &exe_dir, &opts),
        Command::Rollback => rollback(&target),
    }
}

fn parse_args(args: &[String]) -> Result<Options, String> {
    let mut opts = Options::default();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-h" | "--help" => {
                print_usage();
                std::process::exit(0);
            }
            "install" => opts.command = Command::Install,
            "rollback" => opts.command = Command::Rollback,
            "--target" => opts.target = Some(PathBuf::from(next_value(&mut it, "--target")?)),
            s if s.starts_with("--target=") => {
                opts.target = Some(PathBuf::from(&s["--target=".len()..]));
            }
            "--dll" => opts.dll = Some(PathBuf::from(next_value(&mut it, "--dll")?)),
            s if s.starts_with("--dll=") => opts.dll = Some(PathBuf::from(&s["--dll=".len()..])),
            "--xdelta" => opts.xdelta = Some(PathBuf::from(next_value(&mut it, "--xdelta")?)),
            s if s.starts_with("--xdelta=") => {
                opts.xdelta = Some(PathBuf::from(&s["--xdelta=".len()..]));
            }
            other => return Err(format!("unknown argument {other:?} (try --help)")),
        }
    }
    Ok(opts)
}

fn next_value<'a>(it: &mut impl Iterator<Item = &'a String>, flag: &str) -> Result<String, String> {
    it.next()
        .cloned()
        .ok_or_else(|| format!("{flag} requires a value"))
}

fn print_usage() {
    println!("xdelta3-installer - install/rollback the xdelta3-wrap launcher DLL");
    println!();
    println!("Usage: xdelta3-installer [COMMAND] [OPTIONS]");
    println!();
    println!("Commands:");
    println!("  install    Replace XDelta3WrapFactory.dll with xdelta3_wrap.dll and");
    println!("             install xdelta.exe into Launcher3Modules/ (default)");
    println!("  rollback   Restore the original DLL and remove installed files");
    println!();
    println!("Options:");
    println!("  --target <dir>   Launcher directory (default: this exe's directory)");
    println!("  --dll <path>     xdelta3_wrap.dll source, used when the artifact is not embedded");
    println!("  --xdelta <path>  xdelta.exe source, used when the artifact is not embedded");
    println!("  -h, --help       Show this help");
}

fn install(target: &Path, exe_dir: &Path, opts: &Options) -> Result<(), String> {
    let config_path = target.join(LAUNCHER_CONFIG_REL);
    if !config_path.is_file() {
        return Err(format!(
            "target does not look like a launcher directory: {} not found",
            config_path.display()
        ));
    }

    let arts = artifact::resolve_artifacts(
        embedded::DLL,
        embedded::XDELTA,
        opts.dll.as_deref(),
        opts.xdelta.as_deref(),
        exe_dir,
    )?;

    // Check if already installed (backup exists).
    let bak_path = backup::backup_path(target, XDELTA_SRC_REL);
    if bak_path.is_file() {
        return Err("already installed; run \"xdelta3-installer rollback\" first".to_string());
    }

    // Load any previous manifest before mutation.
    let prev = backup::load_manifest(target)?;
    let prev_installed = prev
        .as_ref()
        .map(|m| m.installed.clone())
        .unwrap_or_default();

    // Create backup directory and rename original DLL → .bak.
    std::fs::create_dir_all(backup::backup_dir(target))
        .map_err(|e| format!("create backup dir: {e}"))?;
    let src_path = target.join(XDELTA_SRC_REL);
    if src_path.is_file() {
        backup::backup_file(target, XDELTA_SRC_REL)
            .map_err(|e| format!("back up {}: {e}", src_path.display()))?;
        std::fs::rename(&src_path, &bak_path)
            .map_err(|e| format!("rename {}: {e}", src_path.display()))?;
        println!("backed up {}", src_path.display());
    }

    // Write new DLL.
    if let Some(parent) = src_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    std::fs::write(&src_path, &arts.dll)
        .map_err(|e| format!("write {}: {e}", src_path.display()))?;
    println!("installed {}", src_path.display());

    // Install xdelta.exe (merge with previous installed list, skip if already present).
    let mut installed = prev_installed;
    let xdelta_dest = target.join(XDELTA_DEST_REL);
    if !installed.iter().any(|s| s == XDELTA_DEST_REL) {
        if let Some(parent) = xdelta_dest.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("create {}: {e}", parent.display()))?;
        }
        std::fs::write(&xdelta_dest, &arts.xdelta)
            .map_err(|e| format!("write {}: {e}", xdelta_dest.display()))?;
        println!("installed {}", xdelta_dest.display());
        installed.push(XDELTA_DEST_REL.to_string());
    }

    // Save manifest.
    let manifest = backup::Manifest {
        installed,
        backups: vec![backup::BackupEntry {
            source_rel: XDELTA_SRC_REL.to_string(),
            backup_rel: backup::backup_rel(XDELTA_SRC_REL),
        }],
    };
    backup::save_manifest(target, &manifest).map_err(|e| format!("write manifest: {e}"))?;

    println!("done; run \"xdelta3-installer rollback\" to restore");
    Ok(())
}

fn rollback(target: &Path) -> Result<(), String> {
    let manifest = backup::load_manifest(target)?.ok_or_else(|| {
        format!(
            "no manifest at {}; nothing to roll back",
            backup::manifest_path(target).display()
        )
    })?;

    // Restore backed-up DLL.
    for entry in &manifest.backups {
        let original = target.join(&entry.source_rel);
        let bak = target.join(&entry.backup_rel);
        if !bak.is_file() {
            return Err(format!(
                "backup missing: {} (cannot restore {})",
                bak.display(),
                original.display()
            ));
        }
        if original.is_file() {
            std::fs::remove_file(&original)
                .map_err(|e| format!("remove {}: {e}", original.display()))?;
        }
        std::fs::rename(&bak, &original)
            .map_err(|e| format!("restore {}: {e}", original.display()))?;
        println!("restored {}", original.display());
    }

    // Remove installed files.
    for rel in &manifest.installed {
        let p = target.join(rel);
        if p.is_file() {
            std::fs::remove_file(&p).map_err(|e| format!("remove {}: {e}", p.display()))?;
            println!("removed {}", p.display());
        }
    }

    // Clear the manifest so "manifest exists" stays a true "installed" flag
    // for the GUI double-click toggle. Backups under .rxdelta3/backup/ are
    // preserved.
    let manifest_p = backup::manifest_path(target);
    if manifest_p.is_file() {
        std::fs::remove_file(&manifest_p)
            .map_err(|e| format!("remove {}: {e}", manifest_p.display()))?;
    }
    println!("rollback complete");
    Ok(())
}

#[cfg(test)]
mod testutil {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    pub struct TempDir {
        pub path: PathBuf,
    }

    impl TempDir {
        pub fn new() -> Self {
            let n = COUNTER.fetch_add(1, Ordering::SeqCst);
            let path = std::env::temp_dir().join(format!(
                "xdelta3-installer-test-{}-{n}",
                std::process::id()
            ));
            std::fs::create_dir_all(&path).unwrap();
            TempDir { path }
        }

        pub fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

#[cfg(test)]
mod cli_tests {
    use super::*;

    fn s(x: &str) -> String {
        x.to_string()
    }

    #[test]
    fn default_command_is_install() {
        let opts = parse_args(&[]).unwrap();
        assert_eq!(opts.command, Command::Install);
        assert_eq!(opts.target, None);
    }

    #[test]
    fn parses_rollback_and_space_value_flags() {
        let opts = parse_args(&[s("rollback"), s("--target"), s("C:\\launcher")]).unwrap();
        assert_eq!(opts.command, Command::Rollback);
        assert_eq!(opts.target, Some(PathBuf::from("C:\\launcher")));
    }

    #[test]
    fn parses_equals_flags() {
        let opts = parse_args(&[
            s("install"),
            s("--target=/x"),
            s("--dll=/a.dll"),
            s("--xdelta=/b.exe"),
        ])
        .unwrap();
        assert_eq!(opts.target, Some(PathBuf::from("/x")));
        assert_eq!(opts.dll, Some(PathBuf::from("/a.dll")));
        assert_eq!(opts.xdelta, Some(PathBuf::from("/b.exe")));
    }

    #[test]
    fn unknown_flag_is_an_error() {
        assert!(parse_args(&[s("--bogus")]).is_err());
    }
}
