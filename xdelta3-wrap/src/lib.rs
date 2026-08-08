//! Drop-in replacement for `XDelta3WrapFactory.dll` (FFXIV V3 launcher).
//!
//! Exports are `__stdcall` (x86 only); this crate is only built for
//! `i686-pc-windows-msvc`. All four Merge* functions are implemented; the
//! Generate*/ClassObj exports are empty stubs that keep the same signatures
//! and ordinal slots as the original DLL.

mod dat;
mod log;
mod merge;
mod msgs;
mod win32;

#[no_mangle]
pub extern "stdcall" fn GenerateDeltaDir(
    _a: *const u16,
    _b: *const u16,
    _c: *const u16,
    _d: *const u16,
) -> i32 {
    0
}

#[no_mangle]
pub extern "stdcall" fn GenerateDeltaDirCustomDiff(
    _a: *const u16,
    _b: *const u16,
    _c: *const u16,
    _d: *const u16,
    _e: *const u16,
    _f: *const u16,
    _g: *const u16,
    _h: *const u16,
) -> i32 {
    0
}

#[no_mangle]
pub extern "stdcall" fn GenerateDeltaFile(
    _a: *const u16,
    _b: *const u16,
    _c: *const u16,
) -> i32 {
    0
}

#[no_mangle]
pub extern "stdcall" fn GetClassObj(_guid: *const u8, _pp_obj: *mut *mut ()) -> i32 {
    0
}

#[no_mangle]
pub extern "stdcall" fn ReleaseClassObj(_guid: *const u8, _pp_obj: *mut *mut ()) -> i32 {
    1
}

#[no_mangle]
pub extern "stdcall" fn MergeFile(
    src: *const u16,
    delta: *const u16,
    dst: *const u16,
) -> i32 {
    crate::logln!(
        "[entry] MergeFile src={} delta={} dst={}",
        log::u16_to_lossy(src),
        log::u16_to_lossy(delta),
        log::u16_to_lossy(dst),
    );
    match merge::merge_file(src, delta, dst, None) {
        Ok(()) => 1,
        Err(_) => 0,
    }
}

#[no_mangle]
pub extern "stdcall" fn MergeDir(
    src_dir: *const u16,
    patch_dir: *const u16,
    dst_dir: *const u16,
    file_cb: *const (),
) -> i32 {
    crate::logln!(
        "[entry] MergeDir src_dir={} patch_dir={} dst_dir={}",
        log::u16_to_lossy(src_dir),
        log::u16_to_lossy(patch_dir),
        log::u16_to_lossy(dst_dir),
    );
    merge::merge_dir(src_dir, patch_dir, dst_dir, file_cb, std::ptr::null(), std::ptr::null())
        as i32
}

#[no_mangle]
pub extern "stdcall" fn MergeDirCustomDiff(
    src_dir: *const u16,
    patch_dir: *const u16,
    dst_dir: *const u16,
    file_cb: *const (),
    merge_cb: *const (),
) -> i32 {
    crate::logln!(
        "[entry] MergeDirCustomDiff src_dir={} patch_dir={} dst_dir={}",
        log::u16_to_lossy(src_dir),
        log::u16_to_lossy(patch_dir),
        log::u16_to_lossy(dst_dir),
    );
    merge::merge_dir(src_dir, patch_dir, dst_dir, file_cb, std::ptr::null(), merge_cb) as i32
}

#[no_mangle]
pub extern "stdcall" fn MergeDirCustomDiffV2(
    src_dir: *const u16,
    patch_dir: *const u16,
    dst_dir: *const u16,
    file_cb: *const (),
    cmd_cb: *const (),
    merge_cb: *const (),
) -> i32 {
    crate::logln!(
        "[entry] MergeDirCustomDiffV2 src_dir={} patch_dir={} dst_dir={}",
        log::u16_to_lossy(src_dir),
        log::u16_to_lossy(patch_dir),
        log::u16_to_lossy(dst_dir),
    );
    merge::merge_dir(src_dir, patch_dir, dst_dir, file_cb, cmd_cb, merge_cb) as i32
}
