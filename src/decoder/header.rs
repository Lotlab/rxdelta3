use crate::decoder::address::AddrCache;
use crate::decoder::code_table::{
    CodeTable, default_code_table, deserialize_table, serialize_table,
};
use crate::decoder::instruction::decode_window;
use crate::errors::{Error, Result};
use crate::varint::{read_usize, varint_len};

pub const VCD_SECONDARY: u8 = 1;
pub const VCD_CODETABLE: u8 = 2;
pub const VCD_APPHEADER: u8 = 4;
const VCD_INVHDR: u8 = !0x07;

pub const VCD_DJW_ID: u8 = 1;
pub const VCD_LZMA_ID: u8 = 2;
pub const VCD_FGK_ID: u8 = 16;

pub const MAGIC: [u8; 4] = [0xD6, 0xC3, 0xC4, 0x00];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SecondaryId {
    Djw,
    Lzma,
    Fgk,
}

pub struct Header {
    pub secondary: Option<SecondaryId>,
    pub code_table: CodeTable,
}

pub fn parse_header(delta: &[u8], pos: &mut usize) -> Result<Header> {
    if delta.get(*pos..*pos + 4) != Some(&MAGIC[..]) {
        return Err(Error::Format("bad vcdiff magic"));
    }
    *pos += 4;

    let hdr_ind = *delta
        .get(*pos)
        .ok_or(Error::Format("missing header indicator"))?;
    *pos += 1;
    if hdr_ind & VCD_INVHDR != 0 {
        return Err(Error::Format("invalid header indicator bits"));
    }

    let secondary = if hdr_ind & VCD_SECONDARY != 0 {
        let id = *delta
            .get(*pos)
            .ok_or(Error::Format("missing secondary id"))?;
        *pos += 1;
        Some(match id {
            VCD_DJW_ID => SecondaryId::Djw,
            VCD_LZMA_ID => SecondaryId::Lzma,
            VCD_FGK_ID => SecondaryId::Fgk,
            _ => return Err(Error::Unsupported("unknown secondary compressor")),
        })
    } else {
        None
    };

    let code_table = if hdr_ind & VCD_CODETABLE != 0 {
        let len = read_usize(delta, pos)?;
        let end = pos
            .checked_add(len)
            .ok_or(Error::Format("code table length overflow"))?;
        if end > delta.len() {
            return Err(Error::Format("code table data exceeds input"));
        }
        let table = decode_code_table(&delta[*pos..end])?;
        *pos = end;
        table
    } else {
        default_code_table()
    };

    if hdr_ind & VCD_APPHEADER != 0 {
        let len = read_usize(delta, pos)?;
        let end = pos
            .checked_add(len)
            .ok_or(Error::Format("appheader length overflow"))?;
        if end > delta.len() {
            return Err(Error::Format("appheader exceeds input"));
        }
        *pos = end;
    }

    Ok(Header {
        secondary,
        code_table,
    })
}

fn decode_code_table(data: &[u8]) -> Result<CodeTable> {
    let mut pos = 0usize;
    let s_near = *data
        .get(pos)
        .ok_or(Error::Format("missing near cache size"))? as usize;
    pos += 1;
    let s_same = *data
        .get(pos)
        .ok_or(Error::Format("missing same cache size"))? as usize;
    pos += 1;
    let delta_data = &data[pos..];

    let default = default_code_table();
    let default_bytes = serialize_table(&default);
    let mut target = Vec::new();
    let mut cache = AddrCache::new(default.s_near, default.s_same);

    parse_delta_encoding(
        delta_data,
        1536,
        &default_bytes,
        &default,
        &mut cache,
        &mut target,
    )?;
    if target.len() != 1536 {
        return Err(Error::Format("code table delta produced wrong size"));
    }
    deserialize_table(&target, s_near, s_same)
}

pub fn parse_delta_encoding<'a>(
    data: &'a [u8],
    target_len: usize,
    source_seg: &'a [u8],
    code_table: &CodeTable,
    cache: &mut AddrCache,
    target: &mut Vec<u8>,
) -> Result<(&'a [u8], &'a [u8], &'a [u8])> {
    let mut pos = 0usize;
    let enclen = read_usize(data, &mut pos)?;
    if enclen > data.len() - pos {
        return Err(Error::Format("delta encoding length exceeds input"));
    }
    let end = pos + enclen;

    let declared_target = read_usize(data, &mut pos)?;
    if declared_target != target_len {
        return Err(Error::Format("code table delta target length mismatch"));
    }

    let del_ind = *data
        .get(pos)
        .ok_or(Error::Format("missing delta indicator"))?;
    pos += 1;
    if del_ind != 0 {
        return Err(Error::Format("code table delta must not be compressed"));
    }

    let data_len = read_usize(data, &mut pos)?;
    let inst_len = read_usize(data, &mut pos)?;
    let addr_len = read_usize(data, &mut pos)?;

    let expected = 1
        + varint_len(target_len as u64)
        + varint_len(data_len as u64)
        + varint_len(inst_len as u64)
        + varint_len(addr_len as u64)
        + data_len
        + inst_len
        + addr_len;
    if enclen != expected {
        return Err(Error::Format("incorrect code table delta length"));
    }

    let data_start = pos;
    let inst_start = data_start + data_len;
    let addr_start = inst_start + inst_len;
    let addr_end = addr_start + addr_len;
    if addr_end > end || addr_end > data.len() {
        return Err(Error::Format("code table delta sections exceed input"));
    }

    cache.reset();
    decode_window(
        code_table,
        cache,
        target,
        source_seg,
        &data[inst_start..addr_start],
        &data[data_start..inst_start],
        &data[addr_start..addr_end],
        target_len,
    )?;

    Ok((
        &data[data_start..inst_start],
        &data[inst_start..addr_start],
        &data[addr_start..addr_end],
    ))
}
