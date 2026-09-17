use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::PathBuf;

use clap::{Parser, Subcommand};

use rxdelta::{ApplyOutcome, ApplyStats, ChecksumAlgo};

#[derive(Parser)]
#[command(name = "xdelta", version, about = "Apply VCDIFF/xdelta3 delta patches")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Apply a delta patch to a source file
    Apply(ApplyArgs),
}

#[derive(clap::Args)]
struct ApplyArgs {
    /// Source file (omit for compression-only patches)
    #[arg(short = 's', long)]
    source: Option<PathBuf>,
    /// Delta (patch) file
    delta: PathBuf,
    /// Output file (defaults to stdout)
    #[arg(short = 'o', long)]
    output: Option<PathBuf>,
    /// Skip window checksum verification
    #[arg(long)]
    no_verify: bool,
    /// Print apply statistics to stderr
    #[arg(long)]
    stats: bool,
    /// File checksum algorithm (md5, sha256, blake3; default md5)
    #[arg(long)]
    algo: Option<String>,
    /// Expected source file checksum, hex (enables pre-apply verification and idempotent skip)
    #[arg(long)]
    expect_before: Option<String>,
    /// Expected output checksum, hex (enables skip and post-apply verification)
    #[arg(long)]
    expect_after: Option<String>,
    /// Skip the idempotent source-hash skip check (still verifies output via
    /// --expect-after). Use when the source is known to be the old version.
    #[arg(long)]
    no_skip_check: bool,
    /// Apply the patch in place: rewrite only the changed window slots of the
    /// source file (output == source), skipping unchanged (pure-copy) windows.
    #[arg(long)]
    in_place: bool,
}

struct ChecksumConfig {
    enabled: bool,
    algo: ChecksumAlgo,
    expect_before: Option<Vec<u8>>,
    expect_after: Option<Vec<u8>>,
}

fn parse_checksum_config(args: &ApplyArgs) -> Result<ChecksumConfig, String> {
    let algo = match args.algo.as_deref() {
        None => ChecksumAlgo::Md5,
        Some(s) => ChecksumAlgo::parse(s).ok_or_else(|| format!("unknown algorithm: {s}"))?,
    };
    let parse_hex = |name: &str, s: &str| -> Result<Vec<u8>, String> {
        let bytes =
            rxdelta::checksum::decode_hex(s).ok_or_else(|| format!("{name}: invalid hex value"))?;
        if bytes.len() != algo.digest_len() {
            return Err(format!(
                "{name}: expected {} hex chars for {algo_name}, got {}",
                algo.digest_len() * 2,
                s.len(),
                algo_name = algo.name()
            ));
        }
        Ok(bytes)
    };
    let expect_before = match &args.expect_before {
        Some(s) => Some(parse_hex("--expect-before", s)?),
        None => None,
    };
    let expect_after = match &args.expect_after {
        Some(s) => Some(parse_hex("--expect-after", s)?),
        None => None,
    };
    let enabled = args.algo.is_some() || expect_before.is_some() || expect_after.is_some();
    Ok(ChecksumConfig {
        enabled,
        algo,
        expect_before,
        expect_after,
    })
}

/// Deferred file writer: creates the output file on first write, so that skip /
/// early-failure paths never leave a file behind.
enum OutWriter {
    Stdout(BufWriter<io::Stdout>),
    LazyFile {
        path: PathBuf,
        file: Option<BufWriter<File>>,
    },
}

impl Write for OutWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            OutWriter::Stdout(w) => w.write(buf),
            OutWriter::LazyFile { path, file } => {
                if file.is_none() {
                    *file = Some(BufWriter::with_capacity(1 << 20, File::create(path)?));
                }
                file.as_mut().unwrap().write(buf)
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            OutWriter::Stdout(w) => w.flush(),
            OutWriter::LazyFile { file, .. } => match file {
                Some(f) => f.flush(),
                None => Ok(()),
            },
        }
    }
}

fn main() {
    let cli = Cli::parse();
    let code = match cli.command {
        Commands::Apply(args) => run_apply(&args),
    };
    std::process::exit(code);
}

fn run_apply(args: &ApplyArgs) -> i32 {
    let cfg = match parse_checksum_config(args) {
        Ok(c) => c,
        Err(msg) => {
            eprintln!("error: {msg}");
            return 2;
        }
    };

    let opts = rxdelta::ApplyOptions {
        verify_checksums: !args.no_verify,
        max_window_size: rxdelta::DEFAULT_MAX_WINDOW,
        idempotent_skip: !args.no_skip_check,
    };

    // In-place mode: source file is rewritten directly (output == source).
    if args.in_place {
        return run_apply_in_place(args, &opts, &cfg);
    }

    let mut out = match &args.output {
        Some(path) => OutWriter::LazyFile {
            path: path.clone(),
            file: None,
        },
        None => OutWriter::Stdout(BufWriter::with_capacity(1 << 20, io::stdout())),
    };

    match apply_and_flush(args, &opts, &cfg, &mut out) {
        Ok(stats) => {
            if args.stats {
                eprintln!(
                    "windows: {}, target bytes: {}",
                    stats.windows, stats.target_len
                );
            }
            0
        }
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    }
}

fn run_apply_in_place(args: &ApplyArgs, opts: &rxdelta::ApplyOptions, cfg: &ChecksumConfig) -> i32 {
    let source = match args.source.as_deref() {
        Some(s) => s,
        None => {
            eprintln!("error: --in-place requires a source file");
            return 2;
        }
    };

    // With checksums configured, verify inside the library: `expect_after` is
    // checked before the source is touched, so a mismatch fails with the file
    // still intact (and write-phase I/O failures roll back via the journal).
    if cfg.enabled {
        let outcome = rxdelta::apply_paths_in_place_verified(
            source,
            &args.delta,
            opts,
            cfg.algo,
            cfg.expect_before.as_deref(),
            cfg.expect_after.as_deref(),
        );
        return match outcome {
            Ok(rxdelta::InPlaceOutcome::Applied { stats, checksums }) => {
                print_in_place_stats(args, &stats);
                if let Some(b) = &checksums.before {
                    eprintln!("{}-before: {}", cfg.algo.name(), rxdelta::encode_hex(b));
                }
                if let Some(a) = &checksums.after {
                    eprintln!("{}-after: {}", cfg.algo.name(), rxdelta::encode_hex(a));
                }
                0
            }
            Ok(rxdelta::InPlaceOutcome::Skipped { .. }) => {
                eprintln!("skipped: already patched");
                0
            }
            Err(e) => {
                eprintln!("error: {e}");
                1
            }
        };
    }

    let stats = match rxdelta::apply_paths_in_place(source, &args.delta, opts) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    print_in_place_stats(args, &stats);
    0
}

fn print_in_place_stats(args: &ApplyArgs, stats: &rxdelta::InPlaceStats) {
    if args.stats {
        eprintln!(
            "windows: {}, written: {} ({} bytes), skipped: {}, target: {}",
            stats.windows,
            stats.written_windows,
            stats.written_bytes,
            stats.skipped_windows,
            stats.target_len
        );
    }
}

fn apply_and_flush(
    args: &ApplyArgs,
    opts: &rxdelta::ApplyOptions,
    cfg: &ChecksumConfig,
    out: &mut OutWriter,
) -> rxdelta::Result<ApplyStats> {
    let source = args.source.as_deref();
    if !cfg.enabled {
        let stats = rxdelta::apply_paths(source, &args.delta, out, opts)?;
        out.flush()?;
        return Ok(stats);
    }

    match rxdelta::apply_paths_verified(
        source,
        &args.delta,
        out,
        cfg.algo,
        cfg.expect_before.as_deref(),
        cfg.expect_after.as_deref(),
        opts,
    )? {
        ApplyOutcome::Applied { stats, checksums } => {
            if let Some(b) = &checksums.before {
                eprintln!("{}-before: {}", cfg.algo.name(), rxdelta::encode_hex(b));
            }
            if let Some(a) = &checksums.after {
                eprintln!("{}-after: {}", cfg.algo.name(), rxdelta::encode_hex(a));
            }
            out.flush()?;
            Ok(stats)
        }
        ApplyOutcome::Skipped { .. } => {
            eprintln!("skipped: already patched");
            Ok(ApplyStats::default())
        }
    }
}
