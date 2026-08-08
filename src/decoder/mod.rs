pub mod address;
pub mod checksum;
pub mod code_table;
pub mod header;
pub mod in_place;
pub mod instruction;
pub mod secondary;
pub mod window;

use std::io::Write;

use crate::errors::{Error, Result};
use crate::{ApplyOptions, ApplyStats};

use self::address::AddrCache;
use self::code_table::CodeTable;
use self::header::{SecondaryId, parse_header};
use self::instruction::decode_window;
use self::secondary::{SecondaryDecoder, SectionKind};
use self::window::{Window, parse_window};

pub fn decode_all(
    delta: &[u8],
    source: Option<&[u8]>,
    out: &mut impl Write,
    opts: &ApplyOptions,
) -> Result<ApplyStats> {
    let mut pos = 0usize;
    let hdr = parse_header(delta, &mut pos)?;
    if let Some(s) = hdr.secondary
        && s != SecondaryId::Lzma
    {
        return Err(Error::Unsupported(
            "only LZMA secondary compression is supported",
        ));
    }
    let code_table = &hdr.code_table;
    let mut cache = AddrCache::new(code_table.s_near, code_table.s_same);
    let mut target = Vec::new();
    let mut data_buf = Vec::new();
    let mut inst_buf = Vec::new();
    let mut addr_buf = Vec::new();
    let mut secondary = SecondaryDecoder::new();
    let mut stats = ApplyStats::default();

    while pos < delta.len() {
        let w = parse_window(delta, &mut pos, hdr.secondary, opts.max_window_size)?;
        decode_one_window(
            delta,
            source,
            &w,
            code_table,
            &mut cache,
            opts,
            &mut secondary,
            &mut data_buf,
            &mut inst_buf,
            &mut addr_buf,
            &mut target,
        )?;
        out.write_all(&target)?;
        stats.windows += 1;
        stats.target_len = stats
            .target_len
            .checked_add(target.len() as u64)
            .ok_or(Error::Format("total target size overflow"))?;
        pos = w.addr_end;
    }

    Ok(stats)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn decode_one_window(
    delta: &[u8],
    source: Option<&[u8]>,
    w: &Window,
    code_table: &CodeTable,
    cache: &mut AddrCache,
    opts: &ApplyOptions,
    secondary: &mut SecondaryDecoder,
    data_buf: &mut Vec<u8>,
    inst_buf: &mut Vec<u8>,
    addr_buf: &mut Vec<u8>,
    target: &mut Vec<u8>,
) -> Result<()> {
    let data_sect: &[u8] = if w.data_comp {
        let raw = &delta[w.data_start..w.inst_start];
        let dec = extract_size(raw, opts.max_window_size)?;
        secondary.decompress(SectionKind::Data, &raw[dec.1..], dec.0, data_buf)?;
        &data_buf[..dec.0]
    } else {
        &delta[w.data_start..w.inst_start]
    };
    let inst_sect: &[u8] = if w.inst_comp {
        let raw = &delta[w.inst_start..w.addr_start];
        let dec = extract_size(raw, opts.max_window_size)?;
        secondary.decompress(SectionKind::Inst, &raw[dec.1..], dec.0, inst_buf)?;
        &inst_buf[..dec.0]
    } else {
        &delta[w.inst_start..w.addr_start]
    };
    let addr_sect: &[u8] = if w.addr_comp {
        let raw = &delta[w.addr_start..w.addr_end];
        let dec = extract_size(raw, opts.max_window_size)?;
        secondary.decompress(SectionKind::Addr, &raw[dec.1..], dec.0, addr_buf)?;
        &addr_buf[..dec.0]
    } else {
        &delta[w.addr_start..w.addr_end]
    };

    let source_seg: &[u8] = if w.win_ind & window::VCD_SOURCE != 0 {
        match source {
            Some(s) => {
                let seg_end = w
                    .source_seg_pos
                    .checked_add(w.source_seg_len)
                    .ok_or(Error::Format("source segment out of range"))?;
                if seg_end > s.len() {
                    return Err(Error::Format("source segment out of range"));
                }
                &s[w.source_seg_pos..seg_end]
            }
            None => return Err(Error::Format("window requires a source file")),
        }
    } else {
        &[]
    };

    cache.reset();
    decode_window(
        code_table,
        cache,
        target,
        source_seg,
        inst_sect,
        data_sect,
        addr_sect,
        w.target_len,
    )?;

    if opts.verify_checksums
        && let Some(expected) = w.checksum
    {
        let actual = checksum::adler32(target);
        if actual != expected {
            return Err(Error::ChecksumMismatch { expected, actual });
        }
    }
    Ok(())
}

fn extract_size(raw: &[u8], max: usize) -> Result<(usize, usize)> {
    let mut p = 0usize;
    let n = crate::varint::read_usize(raw, &mut p)?;
    if n > max {
        return Err(Error::Format("decompressed section exceeds maximum"));
    }
    Ok((n, p))
}

pub fn delta_target_len(delta: &[u8], max_window: usize) -> Result<u64> {
    let mut pos = 0usize;
    let hdr = header::parse_header(delta, &mut pos)?;
    let mut total: u64 = 0;
    while pos < delta.len() {
        let w = window::parse_window(delta, &mut pos, hdr.secondary, max_window)?;
        total = total
            .checked_add(w.target_len as u64)
            .ok_or(Error::Format("target length overflow"))?;
        pos = w.addr_end;
    }
    Ok(total)
}
