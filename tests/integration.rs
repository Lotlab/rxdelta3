use rxdelta::{ApplyOptions, apply};

fn varint(mut v: u64) -> Vec<u8> {
    let mut digits = Vec::new();
    loop {
        digits.push((v & 0x7f) as u8);
        v >>= 7;
        if v == 0 {
            break;
        }
    }
    digits.reverse();
    let mut out = Vec::new();
    for (i, d) in digits.iter().enumerate() {
        if i == digits.len() - 1 {
            out.push(*d);
        } else {
            out.push(*d | 0x80);
        }
    }
    out
}

struct WindowBuilder {
    win_ind: u8,
    src_len: Option<u64>,
    src_pos: Option<u64>,
    target: Vec<u8>,
    data: Vec<u8>,
    inst: Vec<u8>,
    addr: Vec<u8>,
    checksum: bool,
}

impl WindowBuilder {
    fn new() -> Self {
        WindowBuilder {
            win_ind: 0,
            src_len: None,
            src_pos: None,
            target: Vec::new(),
            data: Vec::new(),
            inst: Vec::new(),
            addr: Vec::new(),
            checksum: false,
        }
    }

    fn source(mut self, len: u64, pos: u64) -> Self {
        self.win_ind |= 1;
        self.src_len = Some(len);
        self.src_pos = Some(pos);
        self
    }

    fn checksum(mut self) -> Self {
        self.win_ind |= 4;
        self.checksum = true;
        self
    }

    fn add(mut self, bytes: &[u8]) -> Self {
        self.inst.extend_from_slice(&[1]);
        self.inst.extend_from_slice(&varint(bytes.len() as u64));
        self.data.extend_from_slice(bytes);
        self.target.extend_from_slice(bytes);
        self
    }

    fn run(mut self, byte: u8, n: usize) -> Self {
        self.inst.extend_from_slice(&[0]);
        self.inst.extend_from_slice(&varint(n as u64));
        self.data.push(byte);
        self.target.extend(std::iter::repeat_n(byte, n));
        self
    }

    fn copy(mut self, addr: u64, size: u64) -> Self {
        self.inst.extend_from_slice(&[19]);
        self.inst.extend_from_slice(&varint(size));
        self.addr.extend_from_slice(&varint(addr));
        self.target.resize(self.target.len() + size as usize, 0);
        self
    }

    fn finish(self) -> Vec<u8> {
        let mut out = Vec::new();
        out.push(self.win_ind);
        if let (Some(len), Some(pos)) = (self.src_len, self.src_pos) {
            out.extend_from_slice(&varint(len));
            out.extend_from_slice(&varint(pos));
        }
        let enclen = 1
            + varint(self.target.len() as u64).len()
            + varint(self.data.len() as u64).len()
            + varint(self.inst.len() as u64).len()
            + varint(self.addr.len() as u64).len()
            + self.data.len()
            + self.inst.len()
            + self.addr.len()
            + if self.checksum { 4 } else { 0 };
        out.extend_from_slice(&varint(enclen as u64));
        out.extend_from_slice(&varint(self.target.len() as u64));
        out.push(0);
        out.extend_from_slice(&varint(self.data.len() as u64));
        out.extend_from_slice(&varint(self.inst.len() as u64));
        out.extend_from_slice(&varint(self.addr.len() as u64));
        if self.checksum {
            let cks = rxdelta::decoder::checksum::adler32(&self.target);
            out.extend_from_slice(&cks.to_be_bytes());
        }
        out.extend_from_slice(&self.data);
        out.extend_from_slice(&self.inst);
        out.extend_from_slice(&self.addr);
        out
    }
}

fn finish_table_delta(builder: &WindowBuilder) -> Vec<u8> {
    let mut out = Vec::new();
    let enclen = 1
        + varint(builder.target.len() as u64).len()
        + varint(builder.data.len() as u64).len()
        + varint(builder.inst.len() as u64).len()
        + varint(builder.addr.len() as u64).len()
        + builder.data.len()
        + builder.inst.len()
        + builder.addr.len();
    out.extend_from_slice(&varint(enclen as u64));
    out.extend_from_slice(&varint(builder.target.len() as u64));
    out.push(0);
    out.extend_from_slice(&varint(builder.data.len() as u64));
    out.extend_from_slice(&varint(builder.inst.len() as u64));
    out.extend_from_slice(&varint(builder.addr.len() as u64));
    out.extend_from_slice(&builder.data);
    out.extend_from_slice(&builder.inst);
    out.extend_from_slice(&builder.addr);
    out
}

fn decode(delta: &[u8], source: Option<&[u8]>) -> Result<Vec<u8>, rxdelta::Error> {
    let mut out = Vec::new();
    apply(delta, source, &mut out, &ApplyOptions::default())?;
    Ok(out)
}

#[test]
fn add_only_window() {
    let mut delta = vec![0xD6, 0xC3, 0xC4, 0x00, 0x00];
    delta.extend_from_slice(&WindowBuilder::new().add(b"hello ").add(b"world").finish());
    assert_eq!(decode(&delta, None).unwrap(), b"hello world");
}

#[test]
fn copy_from_source() {
    let mut delta = vec![0xD6, 0xC3, 0xC4, 0x00, 0x00];
    delta.extend_from_slice(
        &WindowBuilder::new()
            .source(8, 4)
            .add(b"XYZ")
            .copy(2, 6)
            .finish(),
    );
    let src = b"0123456789abcdef";
    assert_eq!(decode(&delta, Some(src)).unwrap(), b"XYZ6789ab");
}

#[test]
fn run_and_overlapping_copy() {
    let mut delta = vec![0xD6, 0xC3, 0xC4, 0x00, 0x00];
    delta.extend_from_slice(&WindowBuilder::new().run(b'Z', 5).copy(0, 4).finish());
    assert_eq!(decode(&delta, None).unwrap(), b"ZZZZZZZZZ");
}

#[test]
fn multi_window() {
    let mut delta = vec![0xD6, 0xC3, 0xC4, 0x00, 0x00];
    delta.extend_from_slice(&WindowBuilder::new().add(b"one-").finish());
    delta.extend_from_slice(&WindowBuilder::new().add(b"two").finish());
    assert_eq!(decode(&delta, None).unwrap(), b"one-two");
}

#[test]
fn checksum_verified_and_mismatch() {
    let mut good = vec![0xD6, 0xC3, 0xC4, 0x00, 0x00];
    good.extend_from_slice(&WindowBuilder::new().checksum().add(b"data").finish());
    assert_eq!(decode(&good, None).unwrap(), b"data");

    let mut bad = good.clone();
    let cksum_off = 5 + 7;
    bad[cksum_off] ^= 0xFF;
    let err = decode(&bad, None).unwrap_err();
    assert!(matches!(err, rxdelta::Error::ChecksumMismatch { .. }));
}

#[test]
fn appheader_skipped() {
    let mut delta = vec![0xD6, 0xC3, 0xC4, 0x00, 0x04];
    let app = b"custom app header / data";
    delta.extend_from_slice(&varint(app.len() as u64));
    delta.extend_from_slice(app);
    delta.extend_from_slice(&WindowBuilder::new().add(b"ok").finish());
    assert_eq!(decode(&delta, None).unwrap(), b"ok");
}

#[test]
fn custom_code_table_identity() {
    let mut delta = vec![0xD6, 0xC3, 0xC4, 0x00, 0x02];
    let inner = WindowBuilder::new().copy(0, 1536);
    let mut table_data = vec![4u8, 3u8];
    table_data.extend_from_slice(&finish_table_delta(&inner));
    delta.extend_from_slice(&varint(table_data.len() as u64));
    delta.extend_from_slice(&table_data);
    delta.extend_from_slice(&WindowBuilder::new().add(b"custom table").finish());
    assert_eq!(decode(&delta, None).unwrap(), b"custom table");
}

#[test]
fn bad_magic() {
    let err = decode(b"nope not a vcdiff", None).unwrap_err();
    assert!(matches!(err, rxdelta::Error::Format(_)));
}

#[test]
fn unsupported_secondary() {
    let mut delta = vec![0xD6, 0xC3, 0xC4, 0x00, 0x01, 0x10];
    delta.extend_from_slice(&WindowBuilder::new().finish());
    let err = decode(&delta, None).unwrap_err();
    assert!(matches!(err, rxdelta::Error::Unsupported(_)));
}

#[test]
fn vcd_target_unsupported() {
    let delta = vec![0xD6, 0xC3, 0xC4, 0x00, 0x00, 0x02, 0x04, 0x00, 0x00];
    let err = decode(&delta, None).unwrap_err();
    assert!(matches!(err, rxdelta::Error::Unsupported(_)));
}
