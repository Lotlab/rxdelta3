#![no_main]
use std::io::Write;

use libfuzzer_sys::fuzz_target;

struct Sink {
    cap: usize,
    written: usize,
}

impl Write for Sink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.written + buf.len() > self.cap {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                "output cap exceeded",
            ));
        }
        self.written += buf.len();
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fuzz_target!(|data: &[u8]| {
    let opts = rxdelta::ApplyOptions {
        verify_checksums: false,
        max_window_size: 1 << 20,
    };
    let _ = rxdelta::apply(data, None, &mut Sink { cap: 1 << 20, written: 0 }, &opts);
    if data.len() > 1 {
        let split = (data[0] as usize) % data.len();
        let (src, delta) = data.split_at(split);
        let _ = rxdelta::apply(delta, Some(src), &mut Sink { cap: 1 << 20, written: 0 }, &opts);
    }
});
