//! `pigeonpost-nest-parser IN OUT` — parse one uploaded file inside its own sandbox.
//!
//! Started by nestd for each upload in a separate gVisor container: no network, the input file
//! mounted read-only at `IN`, an empty output directory at `OUT`, bounded memory, processes and
//! time. It writes `OUT/manifest.json` and `OUT/part-NNNN.txt`; nothing else, and never a name
//! taken from the input. See `pigeonpost_nests::parse`.

use std::io::Read;
use std::path::Path;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 3 {
        eprintln!("usage: pigeonpost-nest-parser IN OUT");
        std::process::exit(2);
    }
    let (input, out) = (Path::new(&args[1]), Path::new(&args[2]));
    let mut data = Vec::new();
    let read = std::fs::File::open(input).and_then(|f| {
        f.take(pigeonpost_nests::parse::MAX_INPUT_BYTES as u64 + 1)
            .read_to_end(&mut data)
    });
    if read.is_err() {
        eprintln!("pigeonpost-nest-parser: cannot read the input");
        std::process::exit(1);
    }
    let manifest = pigeonpost_nests::parse::parse(&data);
    for part in &manifest.parts {
        if std::fs::write(out.join(&part.name), part.text.as_bytes()).is_err() {
            eprintln!("pigeonpost-nest-parser: cannot write output");
            std::process::exit(1);
        }
    }
    let json = serde_json::to_vec(&manifest).unwrap_or_default();
    if std::fs::write(out.join("manifest.json"), json).is_err() {
        eprintln!("pigeonpost-nest-parser: cannot write the manifest");
        std::process::exit(1);
    }
}
