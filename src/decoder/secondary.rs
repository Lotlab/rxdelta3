use xz2::stream::{Action, Status, Stream};

use crate::errors::{Error, Result};

#[derive(Clone, Copy)]
pub enum SectionKind {
    Data,
    Inst,
    Addr,
}

pub struct SecondaryDecoder {
    data: Option<Stream>,
    inst: Option<Stream>,
    addr: Option<Stream>,
}

impl Default for SecondaryDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl SecondaryDecoder {
    pub fn new() -> Self {
        SecondaryDecoder {
            data: None,
            inst: None,
            addr: None,
        }
    }

    pub fn decompress(
        &mut self,
        kind: SectionKind,
        compressed: &[u8],
        dec_size: usize,
        out: &mut Vec<u8>,
    ) -> Result<()> {
        let slot = match kind {
            SectionKind::Data => &mut self.data,
            SectionKind::Inst => &mut self.inst,
            SectionKind::Addr => &mut self.addr,
        };
        if slot.is_none() {
            *slot = Some(
                Stream::new_stream_decoder(u64::MAX, 0)
                    .map_err(|_| Error::Format("lzma init failed"))?,
            );
        }
        let strm = slot.as_mut().unwrap();

        out.clear();
        out.reserve(dec_size);
        let mut in_off = 0usize;
        while in_off < compressed.len() && out.len() < dec_size {
            let in_before = strm.total_in();
            let status = strm
                .process_vec(&compressed[in_off..], out, Action::Run)
                .map_err(|_| Error::Format("lzma decode failed"))?;
            let consumed = (strm.total_in() - in_before) as usize;
            in_off += consumed;
            if out.len() > dec_size {
                out.truncate(dec_size);
                break;
            }
            if status == Status::StreamEnd {
                break;
            }
            if consumed == 0 {
                break;
            }
        }
        if out.len() != dec_size {
            return Err(Error::Format("LZMA output size mismatch"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn compress(data: &[u8]) -> Vec<u8> {
        let mut enc = xz2::write::XzEncoder::new(Vec::new(), 6);
        enc.write_all(data).unwrap();
        enc.finish().unwrap()
    }

    #[test]
    fn lzma_roundtrip() {
        let data = b"hello hello hello xdelta lzma compression test";
        let c = compress(data);
        let mut dec = SecondaryDecoder::new();
        let mut out = Vec::new();
        dec.decompress(SectionKind::Data, &c, data.len(), &mut out)
            .unwrap();
        assert_eq!(out, data);
    }

    #[test]
    fn lzma_continuous_stream() {
        use xz2::stream::{Check, Filters, LzmaOptions};
        let a = b"first block data ".repeat(20);
        let b = b"second block data!".repeat(20);
        let mut filters = Filters::new();
        filters.lzma2(&LzmaOptions::new_preset(6).unwrap());
        let mut enc = Stream::new_stream_encoder(&filters, Check::None).unwrap();
        let mut block_a = Vec::with_capacity(65536);
        enc.process_vec(&a, &mut block_a, Action::Run).unwrap();
        enc.process_vec(&[], &mut block_a, Action::SyncFlush)
            .unwrap();
        let mut block_b = Vec::with_capacity(65536);
        enc.process_vec(&b, &mut block_b, Action::Run).unwrap();
        enc.process_vec(&[], &mut block_b, Action::SyncFlush)
            .unwrap();

        let mut dec = SecondaryDecoder::new();
        let mut out = Vec::new();
        dec.decompress(SectionKind::Data, &block_a, a.len(), &mut out)
            .unwrap();
        assert_eq!(out, a);
        out.clear();
        dec.decompress(SectionKind::Data, &block_b, b.len(), &mut out)
            .unwrap();
        assert_eq!(out, b);
    }

    #[test]
    fn lzma_truncated_stream_ok() {
        let data = b"the quick brown fox jumps over the lazy dog, repeated data ".repeat(100);
        let mut enc = xz2::write::XzEncoder::new(Vec::new(), 6);
        enc.write_all(&data).unwrap();
        let mut c = enc.finish().unwrap();
        c.truncate(c.len() - 8);
        let mut dec = SecondaryDecoder::new();
        let mut out = Vec::new();
        dec.decompress(SectionKind::Data, &c, data.len(), &mut out)
            .unwrap();
        assert_eq!(out, data);
    }

    #[test]
    fn lzma_size_mismatch_errors() {
        let data = b"hello";
        let c = compress(data);
        let mut dec = SecondaryDecoder::new();
        let mut out = Vec::new();
        assert!(dec.decompress(SectionKind::Data, &c, 6, &mut out).is_err());
    }

    #[test]
    fn lzma_bad_data_errors() {
        let mut dec = SecondaryDecoder::new();
        let mut out = Vec::new();
        assert!(
            dec.decompress(SectionKind::Data, &[0xff, 0xff, 0xff, 0xff], 10, &mut out)
                .is_err()
        );
    }
}
