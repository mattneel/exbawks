//! Golden-frame checks against a private title image.
//!
//! No commercial data may live in this repository, so these tests read
//! their inputs from a directory the developer points at and are ignored by
//! default. Run them with the environment set and the ignore filter lifted:
//!
//! ```text
//! $env:EXBAWKS_PRIVATE_FIXTURES = "C:\path\to\fixtures"
//! cargo test -p exbawks-core --test private_goldens -- --ignored
//! ```
//!
//! The directory holds one manifest per title, `<name>.golden`, whose lines
//! are `key = value` pairs:
//!
//! ```text
//! image = C:\games\title\default.xbe
//! max_blocks = 8000000
//! ram_mib = 128
//! frame = e5fd3002468274f8
//! ```
//!
//! A manifest naming an image that is missing is skipped rather than
//! failed: a developer who has one title should not see another's failure.

#![cfg(all(windows, target_arch = "x86_64"))]

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use exbawks_core::{EmulatorBuilder, EmulatorConfig};

/// One manifest's settings.
struct Golden {
    image: PathBuf,
    max_blocks: usize,
    ram_mib: usize,
    frame: String,
    /// The directory mounted as the writable hard disk. A title creates its
    /// save directories there, and refuses to start without one.
    hdd: PathBuf,
}

/// The manifest keys a golden understands, for the module documentation.
const _MANIFEST_KEYS: [&str; 5] = ["image", "max_blocks", "ram_mib", "frame", "hdd"];

/// Reads `key = value` lines, ignoring blanks and `#` comments.
fn parse_manifest(text: &str) -> HashMap<String, String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.trim().to_owned(), value.trim().to_owned()))
        .collect()
}

/// Loads every manifest in the private fixtures directory.
fn goldens() -> Vec<(PathBuf, Golden)> {
    let Some(root) = std::env::var_os("EXBAWKS_PRIVATE_FIXTURES") else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(Path::new(&root)) else {
        return Vec::new();
    };
    let mut goldens = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|extension| extension != "golden") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let fields = parse_manifest(&text);
        let (Some(image), Some(frame)) = (fields.get("image"), fields.get("frame")) else {
            panic!("{} names no image or no frame digest", path.display());
        };
        // The manifest's own name gives the default mount a stable place.
        let default_hdd = std::env::temp_dir().join("exbawks-goldens").join(
            path.file_stem()
                .map_or_else(|| "title".to_owned(), |stem| stem.to_string_lossy().into_owned()),
        );
        goldens.push((
            path,
            Golden {
                image: PathBuf::from(image),
                max_blocks: fields
                    .get("max_blocks")
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(8_000_000),
                ram_mib: fields.get("ram_mib").and_then(|value| value.parse().ok()).unwrap_or(128),
                frame: frame.clone(),
                hdd: fields.get("hdd").map_or(default_hdd, PathBuf::from),
            },
        ));
    }
    goldens
}

#[test]
#[ignore = "requires a private title image; see the module documentation"]
fn private_titles_render_their_recorded_frame() {
    let goldens = goldens();
    assert!(
        !goldens.is_empty(),
        "no manifests found; set EXBAWKS_PRIVATE_FIXTURES to a directory of .golden files"
    );

    for (manifest, golden) in goldens {
        if !golden.image.is_file() {
            eprintln!("skipping {}: {} is missing", manifest.display(), golden.image.display());
            continue;
        }
        let bytes = std::fs::read(&golden.image).expect("the image reads");
        let config = EmulatorConfig {
            physical_memory_bytes: golden.ram_mib * 1024 * 1024,
            ..EmulatorConfig::default()
        };
        let mut emulator = EmulatorBuilder::new().config(config).build().expect("emulator builds");
        if let Some(parent) = golden.image.parent() {
            emulator.set_disc_root(parent.to_path_buf());
        }
        std::fs::create_dir_all(&golden.hdd).expect("the hard-disk mount is creatable");
        emulator.set_hdd_root(golden.hdd.clone());
        emulator.load_xbe(bytes).expect("the image loads");
        let stop = emulator.run_whp(golden.max_blocks).expect("the run completes");

        let frame = emulator
            .capture_frame()
            .unwrap_or_else(|error| panic!("no frame after {stop:?}: {error}"));
        let digest = exbawks_debug::frame_digest(frame.width, frame.height, &frame.pixels);
        assert_eq!(
            digest,
            golden.frame,
            "{} rendered a different frame; record the new digest only after looking at it",
            manifest.display()
        );
    }
}

#[test]
fn a_manifest_parses_its_fields() {
    let fields = parse_manifest(
        "# a title\nimage = C:\\games\\title\\default.xbe\nmax_blocks = 42\n\nframe = abcdef\n",
    );
    assert_eq!(fields.get("image").map(String::as_str), Some("C:\\games\\title\\default.xbe"));
    assert_eq!(fields.get("max_blocks").map(String::as_str), Some("42"));
    assert_eq!(fields.get("frame").map(String::as_str), Some("abcdef"));
    assert_eq!(fields.len(), 3, "comments and blank lines carry no fields");
}

#[test]
fn goldens_are_empty_without_the_environment() {
    // The suite must be inert for a developer who has no private fixtures,
    // which is what keeps `cargo test` runnable from a clean checkout.
    if std::env::var_os("EXBAWKS_PRIVATE_FIXTURES").is_none() {
        assert!(goldens().is_empty());
    }
}
