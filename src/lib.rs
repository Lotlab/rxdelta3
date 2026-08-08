pub mod checksum;
pub mod decoder;
pub mod errors;
pub mod io;
pub mod varint;

pub use checksum::{
    Blake3, BoxedChecksum, Checksum, ChecksumAlgo, HashingWriter, Md5, Sha256, encode_hex,
};
pub use errors::{Error, Result};
pub use decoder::in_place;
pub use decoder::in_place::{
    DEFAULT_IN_PLACE_THRESHOLD, InPlaceStats, Layout, WindowLayout, apply_paths_in_place,
    apply_paths_in_place_with_threshold, scan_layout,
};

use std::io::Write;
use std::path::Path;

pub const DEFAULT_MAX_WINDOW: usize = 1 << 26;

#[derive(Debug, Clone)]
pub struct ApplyOptions {
    pub verify_checksums: bool,
    pub max_window_size: usize,
    /// When `true` (default), `apply_paths_verified` computes the source hash
    /// to detect an already-patched source and skip the apply. When `false`,
    /// the source is never hashed; `--expect-after` still verifies the output.
    /// Disable it when the source is known to be the old version (e.g. a patch
    /// runner that always applies), to avoid a full extra source read.
    pub idempotent_skip: bool,
}

impl Default for ApplyOptions {
    fn default() -> Self {
        ApplyOptions {
            verify_checksums: true,
            max_window_size: DEFAULT_MAX_WINDOW,
            idempotent_skip: true,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ApplyStats {
    pub target_len: u64,
    pub windows: u64,
}

pub fn apply(
    delta: &[u8],
    source: Option<&[u8]>,
    out: &mut impl std::io::Write,
    opts: &ApplyOptions,
) -> Result<ApplyStats> {
    decoder::decode_all(delta, source, out, opts)
}

pub fn apply_paths(
    source: Option<&Path>,
    delta: &Path,
    out: &mut impl Write,
    opts: &ApplyOptions,
) -> Result<ApplyStats> {
    let delta_map = io::MappedFile::open(delta)?;
    let source_map = match source {
        Some(p) => Some(io::MappedFile::open(p)?),
        None => None,
    };
    apply(
        delta_map.as_bytes(),
        source_map.as_ref().map(|m| m.as_bytes()),
        out,
        opts,
    )
}

#[derive(Debug, Clone, Default)]
pub struct FileChecksums {
    pub before: Option<Box<[u8]>>,
    pub after: Option<Box<[u8]>>,
}

#[derive(Debug)]
pub enum ApplyOutcome {
    Applied {
        stats: ApplyStats,
        checksums: FileChecksums,
    },
    Skipped {
        source_checksum: Box<[u8]>,
    },
}

pub fn apply_paths_verified(
    source: Option<&Path>,
    delta: &Path,
    out: &mut impl Write,
    algo: ChecksumAlgo,
    expect_before: Option<&[u8]>,
    expect_after: Option<&[u8]>,
    opts: &ApplyOptions,
) -> Result<ApplyOutcome> {
    let delta_map = io::MappedFile::open(delta)?;
    let source_map = match source {
        Some(p) => Some(io::MappedFile::open(p)?),
        None => None,
    };

    // Source hashing serves two purposes: the `expect_before` integrity check,
    // and the idempotent skip check against `expect_after`. When only
    // `expect_after` is given, the skip check is the only consumer. A source
    // already holding the patched content must have the same size as the patch
    // target, so if the sizes differ the source provably cannot be skipped and
    // hashing it would be wasted work. Parse the delta window headers (cheap,
    // no decompression) to obtain the target size and gate the hash. The whole
    // skip check can also be disabled via `opts.idempotent_skip = false` for
    // callers that always apply (e.g. a patch runner on old sources).
    let source_hash: Option<Box<[u8]>> = {
        let src = source_map.as_ref().map(|m| m.as_bytes());
        let hash_needed = match src {
            None => false,
            Some(_) if expect_before.is_some() => true,
            Some(s) => {
                opts.idempotent_skip
                    && expect_after.is_some()
                    && decoder::delta_target_len(delta_map.as_bytes(), opts.max_window_size)?
                        == s.len() as u64
            }
        };
        if hash_needed {
            let mut h = algo.instantiate();
            h.update(src.unwrap());
            Some(h.digest())
        } else {
            None
        }
    };

    if let (Some(src), Some(after)) = (&source_hash, expect_after)
        && src.as_ref() == after
    {
        return Ok(ApplyOutcome::Skipped {
            source_checksum: src.clone(),
        });
    }

    if let (Some(src), Some(before)) = (&source_hash, expect_before)
        && src.as_ref() != before
    {
        return Err(Error::FileChecksumMismatch {
            phase: "source",
            algo: algo.name(),
            expected: encode_hex(before),
            actual: encode_hex(src),
        });
    }

    let mut hashing = checksum::HashingWriter::new(&mut *out, algo.instantiate());
    let stats = decoder::decode_all(
        delta_map.as_bytes(),
        source_map.as_ref().map(|m| m.as_bytes()),
        &mut hashing,
        opts,
    )?;
    let (_, output_hash) = hashing.finalize();
    let output_digest = output_hash.digest();

    if let Some(after) = expect_after
        && output_digest.as_ref() != after
    {
        return Err(Error::FileChecksumMismatch {
            phase: "output",
            algo: algo.name(),
            expected: encode_hex(after),
            actual: encode_hex(&output_digest),
        });
    }

    Ok(ApplyOutcome::Applied {
        stats,
        checksums: FileChecksums {
            before: source_hash,
            after: Some(output_digest),
        },
    })
}
