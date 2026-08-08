//! Minimal Win32 interop for GUI mode, std-only.
//!
//! On Windows: native `MessageBoxW` result popup and `AttachConsole` so CLI
//! output is visible when launched from a console. Elsewhere: no-ops (the
//! non-Windows `message_box` prints to stdout so behavior stays observable
//! and testable on Linux). The `#![windows_subsystem = "windows"]` crate
//! attribute in main.rs makes the Windows build a GUI app with no console.

/// NUL-terminated UTF-16 encoding of `s` (for MessageBoxW / CreateFileW).
#[cfg(any(windows, test))]
pub fn to_utf16(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(windows)]
mod api {
    use std::os::raw::c_void;

    pub const MB_OK: u32 = 0x0000;
    pub const MB_ICONERROR: u32 = 0x0010;
    pub const MB_ICONINFORMATION: u32 = 0x0040;
    pub const ATTACH_PARENT_PROCESS: u32 = u32::MAX;
    pub const STD_OUTPUT_HANDLE: u32 = u32::MAX - 10;
    pub const STD_ERROR_HANDLE: u32 = u32::MAX - 11;
    pub const GENERIC_WRITE: u32 = 0x4000_0000;
    pub const FILE_SHARE_WRITE: u32 = 0x2;
    pub const OPEN_EXISTING: u32 = 3;
    pub const INVALID_HANDLE_VALUE: isize = -1;

    #[link(name = "user32")]
    extern "system" {
        pub fn MessageBoxW(
            hwnd: *mut c_void,
            lp_text: *const u16,
            lp_caption: *const u16,
            u_type: u32,
        ) -> i32;
    }

    #[link(name = "kernel32")]
    extern "system" {
        pub fn AttachConsole(process_id: u32) -> i32;
        pub fn CreateFileW(
            lp_file_name: *const u16,
            dw_desired_access: u32,
            dw_share_mode: u32,
            lp_security_attributes: *mut c_void,
            dw_creation_disposition: u32,
            dw_flags_and_attributes: u32,
            h_template_file: *mut c_void,
        ) -> isize;
        pub fn GetStdHandle(n_std_handle: u32) -> isize;
        pub fn SetStdHandle(n_std_handle: u32, h_handle: isize) -> i32;
    }
}

/// Show a native message box. `is_error` selects the icon.
#[cfg(windows)]
pub fn message_box(text: &str, title: &str, is_error: bool) {
    let text_u = to_utf16(text);
    let title_u = to_utf16(title);
    let flags = if is_error {
        api::MB_OK | api::MB_ICONERROR
    } else {
        api::MB_OK | api::MB_ICONINFORMATION
    };
    unsafe {
        api::MessageBoxW(
            std::ptr::null_mut(),
            text_u.as_ptr(),
            title_u.as_ptr(),
            flags,
        );
    }
}

/// Re-route stdout/stderr to the parent console (when launched from a
/// terminal) so a GUI-subsystem binary still shows CLI output.
#[cfg(windows)]
pub fn attach_console() {
    unsafe {
        if api::AttachConsole(api::ATTACH_PARENT_PROCESS) == 0 {
            return;
        }
        let conout = to_utf16("CONOUT$");
        let h_out = api::CreateFileW(
            conout.as_ptr(),
            api::GENERIC_WRITE,
            api::FILE_SHARE_WRITE,
            std::ptr::null_mut(),
            api::OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        );
        if h_out == api::INVALID_HANDLE_VALUE {
            return;
        }
        // Only redirect if the current stdout/stderr are invalid (no console).
        // When running under Command::output(), stdout/stderr are valid pipes
        // and should not be redirected to CONOUT$.
        let cur_out = api::GetStdHandle(api::STD_OUTPUT_HANDLE);
        if cur_out == api::INVALID_HANDLE_VALUE || cur_out == 0 {
            api::SetStdHandle(api::STD_OUTPUT_HANDLE, h_out);
        }
        let cur_err = api::GetStdHandle(api::STD_ERROR_HANDLE);
        if cur_err == api::INVALID_HANDLE_VALUE || cur_err == 0 {
            api::SetStdHandle(api::STD_ERROR_HANDLE, h_out);
        }
    }
}

#[cfg(not(windows))]
pub fn message_box(text: &str, title: &str, _is_error: bool) {
    println!("[{title}] {text}");
}

#[cfg(not(windows))]
pub fn attach_console() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_utf16_encodes_ascii_with_nul() {
        assert_eq!(to_utf16("abc"), vec![97, 98, 99, 0]);
    }

    #[test]
    fn to_utf16_encodes_unicode_with_nul() {
        let v = to_utf16("安装成功");
        assert_eq!(v.len(), 5);
        assert_eq!(*v.last().unwrap(), 0);
    }

    #[cfg(not(windows))]
    #[test]
    fn stubs_are_callable() {
        attach_console();
        message_box("x", "y", false);
        message_box("x", "y", true);
    }
}
