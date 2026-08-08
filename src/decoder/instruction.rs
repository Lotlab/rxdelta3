use crate::decoder::address::AddrCache;
use crate::decoder::code_table::{ADD, COPY, CodeTable, DeltaInst, NOOP, RUN};
use crate::errors::{Error, Result};
use crate::varint::read_usize;

#[allow(clippy::too_many_arguments)]
pub fn decode_window(
    code_table: &CodeTable,
    cache: &mut AddrCache,
    target: &mut Vec<u8>,
    source_seg: &[u8],
    inst_sect: &[u8],
    data_sect: &[u8],
    addr_sect: &[u8],
    expected_len: usize,
) -> Result<()> {
    target.clear();
    if target.capacity() < expected_len {
        target.reserve(expected_len - target.capacity());
    }
    let s = source_seg.len();
    let mut inst_pos = 0usize;
    let mut data_pos = 0usize;
    let mut addr_pos = 0usize;

    while inst_pos < inst_sect.len() {
        let op = *inst_sect
            .get(inst_pos)
            .ok_or(Error::Format("missing instruction code"))?;
        inst_pos += 1;
        let entry = &code_table.entries[op as usize];

        if entry.first.inst != NOOP {
            exec_inst(
                &entry.first,
                cache,
                target,
                source_seg,
                s,
                expected_len,
                inst_sect,
                &mut inst_pos,
                data_sect,
                &mut data_pos,
                addr_sect,
                &mut addr_pos,
            )?;
        }
        if entry.second.inst != NOOP {
            exec_inst(
                &entry.second,
                cache,
                target,
                source_seg,
                s,
                expected_len,
                inst_sect,
                &mut inst_pos,
                data_sect,
                &mut data_pos,
                addr_sect,
                &mut addr_pos,
            )?;
        }
    }

    if target.len() != expected_len {
        return Err(Error::Format("target window length mismatch"));
    }
    Ok(())
}

#[inline]
#[allow(clippy::too_many_arguments)]
fn exec_inst(
    di: &DeltaInst,
    cache: &mut AddrCache,
    target: &mut Vec<u8>,
    source_seg: &[u8],
    s: usize,
    expected_len: usize,
    inst_sect: &[u8],
    inst_pos: &mut usize,
    data_sect: &[u8],
    data_pos: &mut usize,
    addr_sect: &[u8],
    addr_pos: &mut usize,
) -> Result<()> {
    let size = if di.size == 0 {
        read_usize(inst_sect, inst_pos)?
    } else {
        di.size as usize
    };

    match di.inst {
        ADD => {
            check_size(size, expected_len, target.len())?;
            let end = data_pos
                .checked_add(size)
                .ok_or(Error::Format("add exceeds data section"))?;
            if end > data_sect.len() {
                return Err(Error::Format("add exceeds data section"));
            }
            target.extend_from_slice(&data_sect[*data_pos..end]);
            *data_pos = end;
        }
        RUN => {
            check_size(size, expected_len, target.len())?;
            let byte = *data_sect
                .get(*data_pos)
                .ok_or(Error::Format("run missing data byte"))?;
            *data_pos += 1;
            target.resize(target.len() + size, byte);
        }
        COPY => {
            check_size(size, expected_len, target.len())?;
            let here = s + target.len();
            let addr = cache.decode_addr(di.mode, here, addr_sect, addr_pos)?;
            copy_block(addr, size, source_seg, s, target)?;
        }
        _ => return Err(Error::Format("invalid instruction type")),
    }
    Ok(())
}

#[inline]
fn check_size(size: usize, expected_len: usize, current: usize) -> Result<()> {
    if size > expected_len.saturating_sub(current) {
        return Err(Error::Format("instruction exceeds target window"));
    }
    Ok(())
}

#[inline]
fn copy_block(
    addr: usize,
    size: usize,
    source_seg: &[u8],
    s: usize,
    target: &mut Vec<u8>,
) -> Result<()> {
    if addr < s {
        let in_source = (s - addr).min(size);
        target.extend_from_slice(&source_seg[addr..addr + in_source]);
        let remaining = size - in_source;
        if remaining > 0 {
            copy_from_target(0, remaining, target)?;
        }
    } else {
        copy_from_target(addr - s, size, target)?;
    }
    Ok(())
}

#[inline]
fn copy_from_target(src: usize, size: usize, target: &mut Vec<u8>) -> Result<()> {
    let dst = target.len();
    if src >= dst {
        return Err(Error::Format("copy reads beyond current target"));
    }
    let new_len = dst
        .checked_add(size)
        .ok_or(Error::Format("target overflow"))?;
    target.resize(new_len, 0);
    if src + size <= dst {
        target.copy_within(src..src + size, dst);
    } else {
        let gap = dst - src;
        if gap == 0 {
            return Err(Error::Format("copy zero gap"));
        }
        let mut done = 0;
        while done < size {
            let n = gap.min(size - done);
            let from = dst + done - gap;
            target.copy_within(from..from + n, dst + done);
            done += n;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decoder::code_table::default_code_table;

    fn win(
        inst: &[u8],
        data: &[u8],
        addr: &[u8],
        source: &[u8],
        expected: usize,
    ) -> Result<Vec<u8>> {
        let ct = default_code_table();
        let mut cache = AddrCache::new(ct.s_near, ct.s_same);
        let mut target = Vec::new();
        decode_window(
            &ct,
            &mut cache,
            &mut target,
            source,
            inst,
            data,
            addr,
            expected,
        )?;
        Ok(target)
    }

    #[test]
    fn simple_add() {
        let out = win(&[6, 7], b"hello world", &[], b"", 11).unwrap();
        assert_eq!(out, b"hello world");
    }

    #[test]
    fn copy_from_source() {
        let out = win(&[21, 20], &[], &[0, 5], b"abcdefghi", 9).unwrap();
        assert_eq!(out, b"abcdefghi");
    }

    #[test]
    fn run_instruction() {
        let out = win(&[0, 5], b"z", &[], b"", 5).unwrap();
        assert_eq!(out, b"zzzzz");
    }

    #[test]
    fn overlapping_copy_periodic() {
        let out = win(&[5, 28], b"abcd", &[0], b"", 16).unwrap();
        let expect = b"abcdabcdabcdabcd".to_vec();
        assert_eq!(out, expect);
    }

    #[test]
    fn double_instruction() {
        let out = win(&[172], b"xxxx", &[0], b"", 8).unwrap();
        assert_eq!(out, b"xxxxxxxx");
    }

    #[test]
    fn add_exceeds_data_errors() {
        assert!(win(&[2, 3], b"ab", &[], b"", 10).is_err());
    }

    #[test]
    fn length_mismatch_errors() {
        assert!(win(&[2, 1], b"ab", &[], b"", 5).is_err());
    }
}
