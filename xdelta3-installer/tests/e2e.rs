use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_xdelta3-installer");

const XDELTA_SRC: &str = "Launcher3Modules/XDelta3WrapFactory.dll";
const XDELTA_EXE: &str = "Launcher3Modules/xdelta.exe";

const LAUNCHER_CONFIG: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<LauncherConfig>
	<Update>
		<ExecModule>UpdaterPList.dll</ExecModule>
		<DiffAlgorithmModName>XDelta3WrapFactory.dll</DiffAlgorithmModName>
	</Update>
</LauncherConfig>
"#;

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new() -> Self {
        static C: AtomicU32 = AtomicU32::new(0);
        let n = C.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!("xdelta3-e2e-{}-{n}", std::process::id()));
        fs::create_dir_all(&path).unwrap();
        TempDir { path }
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn exe_name() -> &'static str {
    if cfg!(windows) {
        "xdelta3-installer.exe"
    } else {
        "xdelta3-installer"
    }
}

/// Run the installer from a temp directory containing the installer and test
/// artifacts. This ensures artifact resolution (next-to-exe) finds the test
/// DLLs instead of the real ones in target/debug/.
fn run_from_dir(dir: &Path, args: &[&str]) -> (bool, String) {
    let bin_dest = dir.join(exe_name());
    if !bin_dest.is_file() {
        fs::copy(BIN, &bin_dest).unwrap();
    }
    let out = Command::new(&bin_dest).args(args).output().unwrap();
    let msg = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), msg)
}

fn make_launcher(root: &Path) {
    fs::create_dir_all(root.join("Launcher3Modules")).unwrap();
    fs::create_dir_all(root.join("Launcher3Configs")).unwrap();
    fs::write(root.join("Launcher3Configs/LauncherConfig.xml"), LAUNCHER_CONFIG).unwrap();
    fs::write(root.join(XDELTA_SRC), b"original dll bytes").unwrap();
    fs::write(root.join("xdelta3_wrap.dll"), b"new dll bytes").unwrap();
    fs::write(root.join("xdelta.exe"), b"dummy exe bytes").unwrap();
}

#[test]
fn install_then_rollback() {
    let td = TempDir::new();
    let launcher = td.path.join("launcher");
    make_launcher(&launcher);

    // Copy installer next to test DLLs so artifact resolution works.
    // Pass --dll and --xdelta to override embedded artifacts.
    let dll_path = launcher.join("xdelta3_wrap.dll");
    let xdelta_path = launcher.join("xdelta.exe");
    let install_args = [
        "install",
        "--target",
        launcher.to_str().unwrap(),
        "--dll",
        dll_path.to_str().unwrap(),
        "--xdelta",
        xdelta_path.to_str().unwrap(),
    ];

    let (ok, msg) = run_from_dir(&launcher, &install_args);
    assert!(ok, "install failed: {msg}");

    // Original DLL was backed up and replaced.
    let src = launcher.join(XDELTA_SRC);
    assert_eq!(fs::read(&src).unwrap(), b"new dll bytes");

    // Backup exists.
    let bak = launcher.join(".rxdelta3/backup/Launcher3Modules__XDelta3WrapFactory.dll");
    assert_eq!(fs::read(&bak).unwrap(), b"original dll bytes");

    // xdelta.exe installed.
    let exe = launcher.join(XDELTA_EXE);
    assert_eq!(fs::read(&exe).unwrap(), b"dummy exe bytes");

    // Config is untouched.
    let cfg = launcher.join("Launcher3Configs/LauncherConfig.xml");
    let cfg_bytes = fs::read(&cfg).unwrap();
    assert!(cfg_bytes
        .windows(b"XDelta3WrapFactory.dll".len())
        .any(|w| w == b"XDelta3WrapFactory.dll"));

    // Manifest written.
    assert!(launcher.join(".rxdelta3/manifest.json").is_file());

    // Idempotent: second install fails with "already installed".
    let (ok2, msg2) = run_from_dir(&launcher, &install_args);
    assert!(!ok2, "second install should fail");
    assert!(msg2.contains("already installed"), "unexpected: {msg2}");

    // Rollback restores original.
    let (ok3, msg3) = run_from_dir(&launcher, &["rollback", "--target", launcher.to_str().unwrap()]);
    assert!(ok3, "rollback failed: {msg3}");
    assert_eq!(fs::read(&src).unwrap(), b"original dll bytes");
    assert!(!exe.exists());
    assert!(!launcher.join(".rxdelta3/manifest.json").exists());
}

#[test]
fn rollback_without_manifest_errors() {
    let td = TempDir::new();
    let launcher = td.path.join("launcher");
    make_launcher(&launcher);
    let (ok, msg) = run_from_dir(&launcher, &["rollback", "--target", launcher.to_str().unwrap()]);
    assert!(!ok);
    assert!(msg.contains("manifest"));
}

#[cfg(not(windows))]
#[test]
fn gui_toggle_installs_then_uninstalls() {
    let td = TempDir::new();
    let launcher = td.path.join("launcher");
    fs::create_dir_all(launcher.join("Launcher3Modules")).unwrap();
    fs::create_dir_all(launcher.join("Launcher3Configs")).unwrap();

    // The double-clicked exe resolves target = its own directory and finds
    // artifacts next to itself, so place everything inside launcher/.
    fs::copy(BIN, launcher.join(exe_name())).unwrap();
    fs::write(launcher.join("xdelta3_wrap.dll"), b"new dll bytes").unwrap();
    fs::write(launcher.join("xdelta.exe"), b"dummy exe bytes").unwrap();
    fs::write(
        launcher.join("Launcher3Configs/LauncherConfig.xml"),
        LAUNCHER_CONFIG,
    )
    .unwrap();
    // Place the original DLL so install has something to rename.
    fs::write(launcher.join(XDELTA_SRC), b"original dll bytes").unwrap();

    let exe = launcher.join(exe_name());
    let src = launcher.join(XDELTA_SRC);

    // 1st double-click (no args): install.
    let out = Command::new(&exe).output().unwrap();
    let ok = out.status.success();
    let msg = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(ok, "toggle install failed: {msg}");
    assert_eq!(fs::read(&src).unwrap(), b"new dll bytes");
    assert!(launcher.join(".rxdelta3/manifest.json").is_file());

    // 2nd double-click (no args): uninstall.
    let out2 = Command::new(&exe).output().unwrap();
    let ok2 = out2.status.success();
    let msg2 = format!(
        "{}{}",
        String::from_utf8_lossy(&out2.stdout),
        String::from_utf8_lossy(&out2.stderr)
    );
    assert!(ok2, "toggle uninstall failed: {msg2}");
    assert_eq!(fs::read(&src).unwrap(), b"original dll bytes");
    assert!(!launcher.join(".rxdelta3/manifest.json").exists());
}
