//! Progress callback message templates, byte-identical to the original DLL.

use crate::win32;

pub const MSG_MERGE_START: &str = "正在合并文件：%s";
pub const MSG_COPY_START: &str = "正在拷贝文件：%s";
pub const MSG_DELETE_START: &str = "正在删除文件：%s";
pub const MSG_VERIFY: &str = "正在检测：%s合并后文件";
pub const MSG_DISK_MERGE: &str = "磁盘空间不够，合并文件：%s失败，导致本";
pub const MSG_DISK_COPY: &str = "磁盘空间不够，拷贝文件：%s失败，导致本";
pub const MSG_MERGE_FAIL: &str = "文件被占用或已损坏，合并文件：%s失败，导致本次";
pub const MSG_COPY_FAIL: &str = "拷贝文件：%s失败，导致本次更新失败！错误码为:";
pub const MSG_DELETE_FAIL: &str = "删除文件：%s失败，导致本次更新失败！错";

/// Fill the single `%s` in a message template with `file`, returning a
/// NUL-terminated UTF-16 buffer. The caller (launcher) reads the callback
/// message as a NUL-terminated wide string, so no fixed-size padding needed.
pub fn format(template: &str, file: &str) -> Vec<u16> {
    let s = if template.contains("%s") {
        template.replacen("%s", file, 1)
    } else {
        template.to_string()
    };
    win32::to_wide(&s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fill_single_placeholder() {
        let w = format(MSG_MERGE_START, "game\\ffxiv_dx11.exe");
        // Drop trailing NUL.
        let utf16: Vec<u16> = w[..w.len() - 1].to_vec();
        let s = String::from_utf16(&utf16).unwrap();
        assert!(s.starts_with("正在合并文件："));
        assert!(s.ends_with("game\\ffxiv_dx11.exe"));
    }

    #[test]
    fn no_placeholder_ok() {
        let w = format(MSG_VERIFY, "x");
        let utf16: Vec<u16> = w[..w.len() - 1].to_vec();
        let s = String::from_utf16(&utf16).unwrap();
        assert!(s.contains("x"));
    }
}
