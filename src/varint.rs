use crate::errors::{Error, Result};

#[inline]
pub fn read_varint(buf: &[u8], pos: &mut usize) -> Result<u64> {
    let mut value: u64 = 0;
    for _ in 0..10u32 {
        let b = *buf
            .get(*pos)
            .ok_or(Error::Format("unexpected end of input"))?;
        *pos += 1;
        if value > (u64::MAX >> 7) {
            return Err(Error::Format("varint overflow"));
        }
        value = (value << 7) | (b & 0x7f) as u64;
        if b & 0x80 == 0 {
            return Ok(value);
        }
    }
    Err(Error::Format("varint overflow"))
}

#[inline]
pub fn read_usize(buf: &[u8], pos: &mut usize) -> Result<usize> {
    let v = read_varint(buf, pos)?;
    usize::try_from(v).map_err(|_| Error::Format("varint too large"))
}

#[inline]
pub fn varint_len(value: u64) -> usize {
    let mut n = 1usize;
    let mut v = value >> 7;
    while v != 0 {
        n += 1;
        v >>= 7;
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(v: u64) {
        let mut buf = Vec::new();
        let mut digits = Vec::new();
        let mut tmp = v;
        loop {
            digits.push((tmp & 0x7f) as u8);
            tmp >>= 7;
            if tmp == 0 {
                break;
            }
        }
        for (i, d) in digits.iter().rev().enumerate() {
            let last = i == digits.len() - 1;
            buf.push(if last { *d } else { *d | 0x80 });
        }
        let mut pos = 0;
        assert_eq!(read_varint(&buf, &mut pos).unwrap(), v);
        assert_eq!(pos, buf.len());
        assert_eq!(varint_len(v), buf.len());
    }

    #[test]
    fn varint_roundtrip() {
        for v in [
            0u64,
            1,
            127,
            128,
            255,
            256,
            16383,
            16384,
            100000,
            u32::MAX as u64,
            u64::MAX,
        ] {
            roundtrip(v);
        }
    }

    #[test]
    fn varint_msb_first_vector() {
        let mut pos = 0;
        assert_eq!(
            read_varint(&[0xBA, 0xEF, 0x9A, 0x15], &mut pos).unwrap(),
            123456789
        );
        assert_eq!(pos, 4);
        let mut pos = 0;
        assert_eq!(read_varint(&[0x86, 0x8D, 0x20], &mut pos).unwrap(), 100000);
        assert_eq!(pos, 3);
    }

    #[test]
    fn varint_truncated() {
        let buf = [0x80u8; 9];
        let mut pos = 0;
        assert!(read_varint(&buf, &mut pos).is_err());
    }

    #[test]
    fn varint_overflow() {
        let mut buf = vec![0xffu8; 10];
        buf[9] = 0x03;
        let mut pos = 0;
        assert!(read_varint(&buf, &mut pos).is_err());
    }
}
