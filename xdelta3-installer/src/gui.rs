//! GUI (double-click) mode: auto-toggle install/uninstall and show a native
//! result popup.

use std::path::Path;

use crate::{backup, win32, Options};

const TITLE: &str = "xdelta3 更新组件安装器";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuiAction {
    Install,
    Uninstall,
}

/// A double-click toggles: installed (manifest present) → uninstall,
/// otherwise → install.
pub fn decide_gui_action(target: &Path) -> GuiAction {
    if backup::manifest_path(target).is_file() {
        GuiAction::Uninstall
    } else {
        GuiAction::Install
    }
}

pub fn result_message(action: GuiAction, result: &Result<(), String>) -> String {
    match result {
        Ok(()) => match action {
            GuiAction::Install => "安装成功".to_string(),
            GuiAction::Uninstall => "卸载成功".to_string(),
        },
        Err(e) => format!("操作失败\n{e}"),
    }
}

pub fn run_gui(target: &Path, exe_dir: &Path) -> Result<(), String> {
    let action = decide_gui_action(target);
    let result = match action {
        GuiAction::Install => crate::install(target, exe_dir, &Options::default()),
        GuiAction::Uninstall => crate::rollback(target),
    };
    let message = result_message(action, &result);
    win32::message_box(&message, TITLE, result.is_err());
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    #[test]
    fn install_when_no_manifest() {
        let td = TempDir::new();
        assert_eq!(decide_gui_action(td.path()), GuiAction::Install);
    }

    #[test]
    fn uninstall_when_manifest_exists() {
        let td = TempDir::new();
        let target = td.path().join("launcher");
        std::fs::create_dir_all(target.join(".rxdelta3")).unwrap();
        std::fs::write(target.join(".rxdelta3/manifest.json"), b"{}").unwrap();
        assert_eq!(decide_gui_action(&target), GuiAction::Uninstall);
    }

    #[test]
    fn install_success_message() {
        assert_eq!(result_message(GuiAction::Install, &Ok(())), "安装成功");
    }

    #[test]
    fn uninstall_success_message() {
        assert_eq!(result_message(GuiAction::Uninstall, &Ok(())), "卸载成功");
    }

    #[test]
    fn failure_message_includes_detail() {
        assert_eq!(
            result_message(GuiAction::Install, &Err("boom".to_string())),
            "操作失败\nboom"
        );
    }
}
