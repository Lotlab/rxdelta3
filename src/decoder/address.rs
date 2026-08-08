use crate::errors::{Error, Result};
use crate::varint::read_usize;

pub struct AddrCache {
    near: Vec<usize>,
    same: Vec<usize>,
    next_slot: usize,
    s_near: usize,
    s_same: usize,
}

impl AddrCache {
    pub fn new(s_near: usize, s_same: usize) -> Self {
        let same = vec![0; s_same * 256];
        AddrCache {
            near: vec![0; s_near],
            same,
            next_slot: 0,
            s_near,
            s_same,
        }
    }

    pub fn reset(&mut self) {
        self.near.fill(0);
        self.same.fill(0);
        self.next_slot = 0;
    }

    #[inline]
    fn update(&mut self, addr: usize) {
        if self.s_near > 0 {
            self.near[self.next_slot] = addr;
            self.next_slot = (self.next_slot + 1) % self.s_near;
        }
        if self.s_same > 0 {
            let idx = addr % (self.s_same * 256);
            self.same[idx] = addr;
        }
    }

    #[inline]
    pub fn decode_addr(
        &mut self,
        mode: u8,
        here: usize,
        addr_sect: &[u8],
        addr_pos: &mut usize,
    ) -> Result<usize> {
        let m = mode as usize;
        let addr = if m == 0 {
            read_usize(addr_sect, addr_pos)?
        } else if m == 1 {
            let d = read_usize(addr_sect, addr_pos)?;
            here.checked_sub(d)
                .ok_or(Error::Format("copy address underflow"))?
        } else if m < 2 + self.s_near {
            let ni = m - 2;
            let d = read_usize(addr_sect, addr_pos)?;
            self.near[ni]
                .checked_add(d)
                .ok_or(Error::Format("copy address overflow"))?
        } else if m < 2 + self.s_near + self.s_same {
            let si = m - (2 + self.s_near);
            let b = *addr_sect
                .get(*addr_pos)
                .ok_or(Error::Format("missing same-cache byte"))?;
            *addr_pos += 1;
            self.same[si * 256 + b as usize]
        } else {
            return Err(Error::Format("invalid address mode"));
        };
        self.update(addr);
        Ok(addr)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sect(bytes: &[u8]) -> Vec<u8> {
        bytes.to_vec()
    }

    #[test]
    fn self_and_here_modes() {
        let mut c = AddrCache::new(4, 3);
        let a = sect(&[0x05]);
        let mut p = 0;
        assert_eq!(c.decode_addr(0, 100, &a, &mut p).unwrap(), 5);
        assert_eq!(p, 1);
        let a = sect(&[0x05]);
        let mut p = 0;
        assert_eq!(c.decode_addr(1, 100, &a, &mut p).unwrap(), 95);
    }

    #[test]
    fn near_mode() {
        let mut c = AddrCache::new(4, 3);
        let a = sect(&[0x10]);
        let mut p = 0;
        assert_eq!(c.decode_addr(0, 1000, &a, &mut p).unwrap(), 16);
        let a = sect(&[0x04]);
        let mut p = 0;
        assert_eq!(c.decode_addr(2, 1000, &a, &mut p).unwrap(), 20);
    }

    #[test]
    fn same_mode() {
        let mut c = AddrCache::new(4, 3);
        let a = sect(&[0x00, 0x40]);
        let mut p = 0;
        assert_eq!(c.decode_addr(0, 1000, &a, &mut p).unwrap(), 0);
        assert_eq!(c.decode_addr(0, 1000, &a, &mut p).unwrap(), 64);
        let a = sect(&[0x40]);
        let mut p = 0;
        assert_eq!(c.decode_addr(6, 1000, &a, &mut p).unwrap(), 64);
    }

    #[test]
    fn here_underflow_errors() {
        let mut c = AddrCache::new(4, 3);
        let a = sect(&[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01]);
        let mut p = 0;
        assert!(c.decode_addr(1, 10, &a, &mut p).is_err());
    }
}
