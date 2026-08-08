use crate::decoder::header::SecondaryId;
use crate::errors::{Error, Result};
use crate::varint::{read_usize, varint_len};

pub const VCD_SOURCE: u8 = 1;
pub const VCD_TARGET: u8 = 2;
pub const VCD_ADLER32: u8 = 4;
const VCD_INVWIN: u8 = !0x07;

pub const VCD_DATACOMP: u8 = 1;
pub const VCD_INSTCOMP: u8 = 2;
pub const VCD_ADDRCOMP: u8 = 4;
const VCD_INVDEL: u8 = !0x07;

#[derive(Debug, Clone, Copy)]
pub struct Window {
    pub win_ind: u8,
    pub source_seg_len: usize,
    pub source_seg_pos: usize,
    pub target_len: usize,
    pub checksum: Option<u32>,
    pub data_len: usize,
    pub inst_len: usize,
    pub addr_len: usize,
    pub data_comp: bool,
    pub inst_comp: bool,
    pub addr_comp: bool,
    pub data_start: usize,
    pub inst_start: usize,
    pub addr_start: usize,
    pub addr_end: usize,
}

pub fn parse_window(
    delta: &[u8],
    pos: &mut usize,
    secondary: Option<SecondaryId>,
    max_window: usize,
) -> Result<Window> {
    let win_ind = *delta
        .get(*pos)
        .ok_or(Error::Format("missing window indicator"))?;
    *pos += 1;
    if win_ind & VCD_INVWIN != 0 {
        return Err(Error::Format("invalid window indicator bits"));
    }
    if win_ind & VCD_TARGET != 0 {
        return Err(Error::Unsupported("VCD_TARGET windows are not supported"));
    }

    let (source_seg_len, source_seg_pos) = if win_ind & VCD_SOURCE != 0 {
        (read_usize(delta, pos)?, read_usize(delta, pos)?)
    } else {
        (0, 0)
    };

    let enclen = read_usize(delta, pos)?;
    if enclen > delta.len() - *pos {
        return Err(Error::Format("delta encoding length exceeds input"));
    }
    let end = *pos + enclen;

    let target_len = read_usize(delta, pos)?;
    if target_len > max_window {
        return Err(Error::Format("target window exceeds maximum"));
    }

    let del_ind = *delta
        .get(*pos)
        .ok_or(Error::Format("missing delta indicator"))?;
    *pos += 1;
    if del_ind & VCD_INVDEL != 0 {
        return Err(Error::Format("invalid delta indicator bits"));
    }
    if del_ind != 0 && secondary.is_none() {
        return Err(Error::Format(
            "delta indicator set without secondary compressor",
        ));
    }

    let data_len = read_usize(delta, pos)?;
    let inst_len = read_usize(delta, pos)?;
    let addr_len = read_usize(delta, pos)?;

    let checksum = if win_ind & VCD_ADLER32 != 0 {
        let b = delta
            .get(*pos..*pos + 4)
            .ok_or(Error::Format("missing checksum"))?;
        *pos += 4;
        Some(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    } else {
        None
    };

    let expected = 1usize
        .checked_add(varint_len(target_len as u64))
        .and_then(|v| v.checked_add(varint_len(data_len as u64)))
        .and_then(|v| v.checked_add(varint_len(inst_len as u64)))
        .and_then(|v| v.checked_add(varint_len(addr_len as u64)))
        .and_then(|v| v.checked_add(data_len))
        .and_then(|v| v.checked_add(inst_len))
        .and_then(|v| v.checked_add(addr_len))
        .and_then(|v| {
            if win_ind & VCD_ADLER32 != 0 {
                v.checked_add(4)
            } else {
                Some(v)
            }
        })
        .ok_or(Error::Format("delta encoding length overflow"))?;
    if enclen != expected {
        return Err(Error::Format("incorrect delta encoding length"));
    }

    let data_start = *pos;
    let inst_start = data_start
        .checked_add(data_len)
        .ok_or(Error::Format("section overflow"))?;
    let addr_start = inst_start
        .checked_add(inst_len)
        .ok_or(Error::Format("section overflow"))?;
    let addr_end = addr_start
        .checked_add(addr_len)
        .ok_or(Error::Format("section overflow"))?;
    if addr_end > end || end > delta.len() {
        return Err(Error::Format("sections exceed delta encoding"));
    }

    Ok(Window {
        win_ind,
        source_seg_len,
        source_seg_pos,
        target_len,
        checksum,
        data_len,
        inst_len,
        addr_len,
        data_comp: del_ind & VCD_DATACOMP != 0,
        inst_comp: del_ind & VCD_INSTCOMP != 0,
        addr_comp: del_ind & VCD_ADDRCOMP != 0,
        data_start,
        inst_start,
        addr_start,
        addr_end,
    })
}
