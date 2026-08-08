//! Minimal Win32 FFI declarations (kernel32) used by the wrapper.
//! On 32-bit x86, `extern "system"` maps to the `stdcall` calling convention
//! used by the Win32 API, matching the original DLL's behavior.

#![allow(non_camel_case_types)]
#![allow(non_snake_case)]
#![allow(dead_code)]

pub type BOOL = i32;
pub type DWORD = u32;
pub type LPCWSTR = *const u16;
pub type ULONGLONG = u64;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ULARGE_INTEGER {
    pub low: DWORD,
    pub high: DWORD,
}

impl ULARGE_INTEGER {
    pub fn quad_part(&self) -> ULONGLONG {
        ((self.high as ULONGLONG) << 32) | self.low as ULONGLONG
    }
}

#[link(name = "kernel32")]
extern "system" {
    pub fn GetDiskFreeSpaceExW(
        lp_directory: LPCWSTR,
        free_bytes_to_caller: *mut ULARGE_INTEGER,
        total_bytes: *mut ULARGE_INTEGER,
        free_bytes: *mut ULARGE_INTEGER,
    ) -> BOOL;
}

// Rust std's file APIs operate on OsStr; we bridge via encode_wide.
pub use std::os::windows::ffi::OsStrExt as _;

pub fn to_wide(s: &str) -> Vec<u16> {
    std::ffi::OsStr::new(s).encode_wide().chain(Some(0)).collect()
}