use std::fmt;

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    Format(&'static str),
    Unsupported(&'static str),
    ChecksumMismatch {
        expected: u32,
        actual: u32,
    },
    FileChecksumMismatch {
        phase: &'static str,
        algo: &'static str,
        expected: String,
        actual: String,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "io error: {e}"),
            Error::Format(msg) => write!(f, "invalid vcdiff data: {msg}"),
            Error::Unsupported(msg) => write!(f, "unsupported: {msg}"),
            Error::ChecksumMismatch { expected, actual } => {
                write!(
                    f,
                    "target window checksum mismatch: expected {expected:08x}, got {actual:08x}"
                )
            }
            Error::FileChecksumMismatch {
                phase,
                algo,
                expected,
                actual,
            } => {
                write!(
                    f,
                    "file checksum mismatch ({phase}): expected {expected}, got {actual} ({algo})"
                )
            }
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_checksum_mismatch_display() {
        let e = Error::FileChecksumMismatch {
            phase: "source",
            algo: "md5",
            expected: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            actual: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into(),
        };
        let s = e.to_string();
        assert!(s.contains("source"), "missing phase: {s}");
        assert!(s.contains("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"));
        assert!(s.contains("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"));
        assert!(s.contains("md5"));
    }

    #[test]
    fn window_checksum_still_hex_8() {
        let e = Error::ChecksumMismatch {
            expected: 0x12345678,
            actual: 0x87654321,
        };
        assert!(e.to_string().contains("12345678"));
        assert!(e.to_string().contains("87654321"));
    }
}
