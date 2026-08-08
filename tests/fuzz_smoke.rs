use std::io::Write;

use rxdelta::{ApplyOptions, apply};

struct Sink {
    cap: usize,
    written: usize,
}

impl Write for Sink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.written + buf.len() > self.cap {
            return Err(std::io::Error::other("output cap exceeded"));
        }
        self.written += buf.len();
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

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

fn seed_delta() -> Vec<u8> {
    let mut d = vec![0xD6, 0xC3, 0xC4, 0x00, 0x00];
    d.extend_from_slice(&varint(32));
    d.extend_from_slice(&varint(8));
    d.push(0);
    d.extend_from_slice(&varint(8));
    d.extend_from_slice(&varint(3));
    d.extend_from_slice(&varint(1));
    d.extend_from_slice(b"01234567");
    d.extend_from_slice(&[19, 1, 0]);
    d.push(0);
    d
}

fn run(delta: &[u8], source: Option<&[u8]>) {
    let opts = ApplyOptions {
        verify_checksums: false,
        max_window_size: 1 << 20,
        idempotent_skip: true,
    };
    let _ = apply(
        delta,
        source,
        &mut Sink {
            cap: 1 << 20,
            written: 0,
        },
        &opts,
    );
}

#[test]
fn random_mutation_no_panic() {
    let seed = seed_delta();
    let mut rng = 0x9E3779B97F4A7C15u64;
    let mut next = move || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng
    };
    let mut buf = seed.clone();
    for i in 0..50000u64 {
        let mode = next() % 5;
        match mode {
            0 => {
                if !buf.is_empty() {
                    let idx = (next() as usize) % buf.len();
                    buf[idx] = next() as u8;
                }
            }
            1 => {
                let start = (next() as usize) % (buf.len() + 1);
                let len = (next() % 64) as usize;
                buf.truncate(start);
                buf.extend((0..len).map(|_| next() as u8));
            }
            2 => {
                let start = (next() as usize) % (buf.len() + 1);
                let len = (next() % 32) as usize;
                let insert = (0..len).map(|_| next() as u8).collect::<Vec<_>>();
                buf.splice(start..start, insert);
            }
            3 => {
                buf = seed.clone();
            }
            _ => {
                buf = vec![next() as u8; (next() % 256) as usize];
            }
        }
        let src = Some(&seed[..]);
        run(&buf, src);
        run(&buf, None);
        if i % 1000 == 0 {
            buf = seed.clone();
        }
    }
}

#[test]
fn pure_random_no_panic() {
    let mut rng = 0xA5A5A5A5A5A5A5A5u64;
    for _ in 0..20000 {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        let len = (rng % 1024) as usize;
        let data = (0..len).map(|_| rng as u8).collect::<Vec<_>>();
        run(&data, None);
        run(&data, Some(&data));
    }
}
