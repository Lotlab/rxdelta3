const MOD: u32 = 65521;
const NMAX: usize = 5552;

pub fn adler32(data: &[u8]) -> u32 {
    let mut a: u32 = 1;
    let mut b: u32 = 0;
    for chunk in data.chunks(NMAX) {
        for &byte in chunk {
            a += byte as u32;
            b += a;
        }
        a %= MOD;
        b %= MOD;
    }
    (b << 16) | a
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_vectors() {
        assert_eq!(adler32(b""), 1);
        assert_eq!(adler32(b"Wikipedia"), 0x11E60398);
        assert_eq!(
            adler32(b"The quick brown fox jumps over the lazy dog"),
            0x5BDC0FDA
        );
    }

    #[test]
    fn chunked_equals_linear() {
        let data = (0..20000u32)
            .map(|i| (i * 31 % 251) as u8)
            .collect::<Vec<_>>();
        assert_eq!(adler32(&data), adler32(&data));
    }
}
