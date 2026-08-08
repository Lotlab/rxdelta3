pub trait Checksum {
    fn update(&mut self, data: &[u8]);

    fn digest(self) -> Box<[u8]>;
}

pub fn encode_hex(data: &[u8]) -> String {
    let mut s = String::with_capacity(data.len() * 2);
    for b in data {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

pub struct HashingWriter<W, H> {
    inner: W,
    hash: H,
}

impl<W, H> HashingWriter<W, H> {
    pub fn new(inner: W, hash: H) -> Self {
        HashingWriter { inner, hash }
    }

    pub fn get_ref(&self) -> &W {
        &self.inner
    }

    pub fn hash(&self) -> &H {
        &self.hash
    }

    pub fn into_inner(self) -> W {
        self.inner
    }

    pub fn finalize(self) -> (W, H) {
        (self.inner, self.hash)
    }
}

impl<W: std::io::Write, H: Checksum> std::io::Write for HashingWriter<W, H> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.hash.update(&buf[..n]);
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

pub struct Md5(fast_md5::Md5);

impl Md5 {
    pub fn new() -> Self {
        Md5(fast_md5::Md5::new())
    }
}

impl Default for Md5 {
    fn default() -> Self {
        Self::new()
    }
}

impl Checksum for Md5 {
    fn update(&mut self, data: &[u8]) {
        self.0.update(data);
    }

    fn digest(self) -> Box<[u8]> {
        self.0.finalize().to_vec().into_boxed_slice()
    }
}

pub struct Sha256(sha2::Sha256);

impl Sha256 {
    pub fn new() -> Self {
        use sha2::Digest as _;
        Sha256(sha2::Sha256::new())
    }
}

impl Default for Sha256 {
    fn default() -> Self {
        Self::new()
    }
}

impl Checksum for Sha256 {
    fn update(&mut self, data: &[u8]) {
        use sha2::Digest as _;
        self.0.update(data);
    }

    fn digest(self) -> Box<[u8]> {
        use sha2::Digest as _;
        self.0.finalize().to_vec().into_boxed_slice()
    }
}

pub struct Blake3(blake3::Hasher);

impl Blake3 {
    pub fn new() -> Self {
        Blake3(blake3::Hasher::new())
    }
}

impl Default for Blake3 {
    fn default() -> Self {
        Self::new()
    }
}

impl Checksum for Blake3 {
    fn update(&mut self, data: &[u8]) {
        self.0.update(data);
    }

    fn digest(self) -> Box<[u8]> {
        self.0.finalize().as_bytes().to_vec().into_boxed_slice()
    }
}

#[allow(clippy::large_enum_variant)]
pub enum BoxedChecksum {
    Md5(Md5),
    Sha256(Sha256),
    Blake3(Blake3),
}

impl Checksum for BoxedChecksum {
    fn update(&mut self, data: &[u8]) {
        match self {
            BoxedChecksum::Md5(h) => h.update(data),
            BoxedChecksum::Sha256(h) => h.update(data),
            BoxedChecksum::Blake3(h) => h.update(data),
        }
    }

    fn digest(self) -> Box<[u8]> {
        match self {
            BoxedChecksum::Md5(h) => h.digest(),
            BoxedChecksum::Sha256(h) => h.digest(),
            BoxedChecksum::Blake3(h) => h.digest(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChecksumAlgo {
    Md5,
    Sha256,
    Blake3,
}

impl ChecksumAlgo {
    pub fn instantiate(&self) -> BoxedChecksum {
        match self {
            ChecksumAlgo::Md5 => BoxedChecksum::Md5(Md5::new()),
            ChecksumAlgo::Sha256 => BoxedChecksum::Sha256(Sha256::new()),
            ChecksumAlgo::Blake3 => BoxedChecksum::Blake3(Blake3::new()),
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            ChecksumAlgo::Md5 => "md5",
            ChecksumAlgo::Sha256 => "sha256",
            ChecksumAlgo::Blake3 => "blake3",
        }
    }

    pub fn digest_len(&self) -> usize {
        match self {
            ChecksumAlgo::Md5 => 16,
            ChecksumAlgo::Sha256 => 32,
            ChecksumAlgo::Blake3 => 32,
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "md5" => Some(ChecksumAlgo::Md5),
            "sha256" => Some(ChecksumAlgo::Sha256),
            "blake3" => Some(ChecksumAlgo::Blake3),
            _ => None,
        }
    }
}

pub fn decode_hex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    for i in (0..s.len()).step_by(2) {
        let hi = hex_nibble(s.as_bytes()[i])?;
        let lo = hex_nibble(s.as_bytes()[i + 1])?;
        out.push((hi << 4) | lo);
    }
    Some(out)
}

fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn md5_known_vectors() {
        let mut h = Md5::new();
        h.update(b"The quick brown fox jumps over the lazy dog");
        assert_eq!(encode_hex(&h.digest()), "9e107d9d372bb6826bd81d3542a419d6");
        assert_eq!(
            encode_hex(&Md5::new().digest()),
            "d41d8cd98f00b204e9800998ecf8427e"
        );
    }

    #[test]
    fn sha256_known_vectors() {
        let mut h = Sha256::new();
        h.update(b"abc");
        assert_eq!(
            encode_hex(&h.digest()),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            encode_hex(&Sha256::new().digest()),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn blake3_known_vector() {
        let mut h = Blake3::new();
        h.update(b"abc");
        assert_eq!(
            encode_hex(&h.digest()),
            "6437b3ac38465133ffb63b75273a8db548c558465d79db03fd359c6cd5bd9d85"
        );
    }

    #[test]
    fn digest_matches_hex() {
        let mut h = Md5::new();
        h.update(b"streaming");
        let d = h.digest();
        assert_eq!(
            encode_hex(&d),
            d.iter().map(|b| format!("{b:02x}")).collect::<String>()
        );
    }

    #[test]
    fn algo_parse_and_len() {
        assert_eq!(ChecksumAlgo::parse("md5"), Some(ChecksumAlgo::Md5));
        assert_eq!(ChecksumAlgo::parse("sha256"), Some(ChecksumAlgo::Sha256));
        assert_eq!(ChecksumAlgo::parse("blake3"), Some(ChecksumAlgo::Blake3));
        assert_eq!(ChecksumAlgo::parse("crc64"), None);
        assert_eq!(ChecksumAlgo::Md5.digest_len(), 16);
        assert_eq!(ChecksumAlgo::Sha256.digest_len(), 32);
    }

    #[test]
    fn decode_hex_roundtrip() {
        assert_eq!(decode_hex("9e107d9d"), Some(vec![0x9e, 0x10, 0x7d, 0x9d]));
        assert_eq!(decode_hex("0x12"), None);
        assert_eq!(decode_hex("123"), None);
        assert_eq!(decode_hex("zz"), None);
    }

    #[test]
    fn instantiate_all_algos() {
        for algo in [
            ChecksumAlgo::Md5,
            ChecksumAlgo::Sha256,
            ChecksumAlgo::Blake3,
        ] {
            let mut h = algo.instantiate();
            h.update(b"x");
            assert_eq!(h.digest().len(), algo.digest_len());
        }
    }

    #[test]
    fn hashing_writer_matches_one_shot() {
        let data: Vec<u8> = (0..100_000u32).map(|i| (i * 31 % 251) as u8).collect();
        let mut one_shot = Md5::new();
        one_shot.update(&data);

        let mut w = HashingWriter::new(Vec::new(), Md5::new());
        for chunk in data.chunks(7) {
            w.write_all(chunk).unwrap();
        }
        let (inner, hash) = w.finalize();
        assert_eq!(inner, data);
        assert_eq!(encode_hex(&hash.digest()), encode_hex(&one_shot.digest()));
    }

    #[test]
    fn hashing_writer_partial_writes() {
        struct ShortWrite;
        impl std::io::Write for ShortWrite {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                Ok(buf.len().min(3))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let mut w = HashingWriter::new(ShortWrite, Md5::new());
        let n = w.write(b"abcdefgh").unwrap();
        assert_eq!(n, 3);
        let (_, hash) = w.finalize();
        let mut expect = Md5::new();
        expect.update(b"abc");
        assert_eq!(encode_hex(&hash.digest()), encode_hex(&expect.digest()));
    }

    #[test]
    fn hashing_writer_passthrough_error() {
        struct FailWriter;
        impl std::io::Write for FailWriter {
            fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("boom"))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut w = HashingWriter::new(FailWriter, Md5::new());
        let err = w.write(b"data").unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::Other);
    }

    #[test]
    fn hashing_writer_flush_delegates() {
        #[derive(Default)]
        struct FlushCounter {
            flushes: u32,
        }
        impl std::io::Write for FlushCounter {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                self.flushes += 1;
                Ok(())
            }
        }
        let mut inner = FlushCounter::default();
        let mut w = HashingWriter::new(&mut inner, Md5::new());
        w.flush().unwrap();
        w.flush().unwrap();
        assert_eq!(inner.flushes, 2);
    }
}
