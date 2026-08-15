#![forbid(unsafe_code)]

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use clap::{Parser, Subcommand, ValueEnum};
use exbawks_core::{BootPlanReport, EmulatorBuilder, EmulatorConfig, KernelThunkTable};
use exbawks_cpu::{BasicBlockDecoder, DecodeConfig, format_instruction};
use exbawks_platform::probe_host_capabilities;
use exbawks_types::{BackendKind, GuestVa};
use exbawks_xbe::XbeImage;
use serde::Serialize;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(name = "exbawks", version, about = "Exbawks development and inspection tools")]
struct Cli {
    /// Enables JSON output for supported commands.
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Reports host runtime capabilities.
    Doctor,
    /// Parses and describes one XBE file.
    Inspect {
        /// The XBE file path.
        path: PathBuf,
    },
    /// Decodes a hexadecimal 32-bit x86 byte sequence.
    Decode {
        /// The first guest instruction address.
        #[arg(long, default_value = "0x00010000", value_parser = parse_u32)]
        ip: u32,
        /// Hexadecimal instruction bytes.
        #[arg(long)]
        hex: String,
        /// The maximum instruction count.
        #[arg(long, default_value_t = 256)]
        max_instructions: usize,
        /// The maximum decoded byte count.
        #[arg(long, default_value_t = 4096)]
        max_bytes: usize,
    },
    /// Loads an XBE and creates an entry-block translation plan.
    Plan {
        /// The XBE file path.
        path: PathBuf,
        /// The translation backend.
        #[arg(long, value_enum, default_value_t = BackendArg::Direct)]
        backend: BackendArg,
        /// The emulated physical RAM size in MiB.
        #[arg(long, default_value_t = 64)]
        ram_mib: usize,
    },
    /// Reads the terminated kernel import thunk table.
    Thunks {
        /// The XBE file path.
        path: PathBuf,
        /// The maximum entry count.
        #[arg(long, default_value_t = 4096)]
        limit: usize,
    },
    /// Performs the implemented load and planning stages.
    Run {
        /// The XBE file path.
        path: PathBuf,
        /// The emulated physical RAM size in MiB.
        #[arg(long, default_value_t = 64)]
        ram_mib: usize,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum BackendArg {
    Direct,
    Cranelift,
}

impl From<BackendArg> for BackendKind {
    fn from(value: BackendArg) -> Self {
        match value {
            BackendArg::Direct => Self::DirectRewrite,
            BackendArg::Cranelift => Self::Cranelift,
        }
    }
}

#[derive(Debug, Serialize)]
struct DecodeReport {
    start: GuestVa,
    byte_len: usize,
    stop: String,
    instructions: Vec<InstructionReport>,
}

#[derive(Debug, Serialize)]
struct InstructionReport {
    address: GuestVa,
    length: usize,
    text: String,
}

fn main() -> Result<()> {
    initialize_tracing();
    let cli = Cli::parse();

    match cli.command {
        Command::Doctor => doctor(cli.json),
        Command::Inspect { path } => inspect(&path, cli.json),
        Command::Decode { ip, hex, max_instructions, max_bytes } => {
            decode(ip, &hex, max_instructions, max_bytes, cli.json)
        }
        Command::Plan { path, backend, ram_mib } => {
            let report = plan(&path, backend.into(), ram_mib)?;
            print_plan(&report, cli.json)
        }
        Command::Thunks { path, limit } => thunks(&path, limit, cli.json),
        Command::Run { path, ram_mib } => run(&path, ram_mib, cli.json),
    }
}

fn initialize_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt().with_env_filter(filter).try_init();
}

fn doctor(json: bool) -> Result<()> {
    let capabilities = probe_host_capabilities();
    if json {
        print_json(&capabilities)
    } else {
        println!("Operating system:       {}", capabilities.operating_system);
        println!("Architecture:           {}", capabilities.architecture);
        println!("Windows x86-64 target:  {}", yes_no(capabilities.supported_runtime_target));
        println!("Placeholder views:      {}", yes_no(capabilities.placeholder_views));
        println!("FSGSBASE available:     {}", yes_no(capabilities.fsgsbase));
        if !capabilities.supported_runtime_target {
            println!(
                "Runtime status:         unsupported for execution; logic tools remain available"
            );
        }
        Ok(())
    }
}

fn inspect(path: &Path, json: bool) -> Result<()> {
    let bytes = read_file(path)?;
    let image =
        XbeImage::parse(&bytes).with_context(|| format!("failed to parse {}", path.display()))?;

    if json {
        return print_json(&image);
    }

    println!("File:                 {}", path.display());
    println!("File size:            {} bytes", image.file_size);
    println!("Build flavor:         {:?}", image.header.build_flavor);
    println!("Image base:           {}", image.header.base_address);
    println!("Image size:           0x{:08X}", image.header.size_of_image);
    println!("Entry point:          {}", image.header.entry_point);
    println!("Kernel thunk table:   {}", image.header.kernel_thunk_address);
    println!("Sections:             {}", image.sections.len());

    for section in &image.sections {
        println!();
        println!("[{}] {}", section.index, section.name);
        println!(
            "  Virtual:            {} + 0x{:X}",
            section.virtual_address, section.virtual_size
        );
        println!("  Raw:                0x{:X} + 0x{:X}", section.raw_address, section.raw_size);
        println!("  Flags:              {:?}", section.flags);
    }

    Ok(())
}

fn decode(ip: u32, hex: &str, max_instructions: usize, max_bytes: usize, json: bool) -> Result<()> {
    let bytes = parse_hex_bytes(hex)?;
    if bytes.is_empty() {
        bail!("the hexadecimal byte sequence is empty");
    }

    let decoder = BasicBlockDecoder::new(DecodeConfig { max_instructions, max_bytes });
    let block = decoder.decode(GuestVa(ip), &bytes)?;
    let instructions = block
        .instructions
        .iter()
        .map(|instruction| InstructionReport {
            address: GuestVa(u32::try_from(instruction.ip()).unwrap_or(u32::MAX)),
            length: instruction.len(),
            text: format_instruction(instruction),
        })
        .collect();
    let report = DecodeReport {
        start: block.start,
        byte_len: block.byte_len,
        stop: format!("{:?}", block.stop),
        instructions,
    };

    if json {
        return print_json(&report);
    }

    for instruction in &report.instructions {
        println!(
            "{}  {:<32} ; {} byte(s)",
            instruction.address, instruction.text, instruction.length
        );
    }
    println!("Stop: {}", report.stop);
    Ok(())
}

fn plan(path: &Path, backend: BackendKind, ram_mib: usize) -> Result<BootPlanReport> {
    let bytes = read_file(path)?;
    let config = EmulatorConfig {
        physical_memory_bytes: mib_to_bytes(ram_mib)?,
        backend,
        ..EmulatorConfig::default()
    };
    let mut emulator = EmulatorBuilder::new().config(config).build()?;
    emulator.load_xbe(bytes).with_context(|| format!("failed to load {}", path.display()))?;
    Ok(emulator.plan_entry_block()?.report())
}

fn print_plan(report: &BootPlanReport, json: bool) -> Result<()> {
    if json {
        return print_json(report);
    }

    println!("Build flavor:        {:?}", report.build_flavor);
    println!("Image base:          {}", report.image_base);
    println!("Entry point:         {}", report.entry_point);
    println!("Kernel thunk table:  {}", report.kernel_thunk_address);
    println!("Backend:             {:?}", report.backend);
    println!("Compilation state:   {}", report.compilation_state);
    println!(
        "Decoded block:       {} byte(s), {} instruction(s)",
        report.decoded_bytes, report.decoded_instructions
    );
    println!("Block stop:          {}", report.block_stop);

    for action in &report.actions {
        println!(
            "{}  {:<32} {:<16} {} byte(s)",
            action.address, action.instruction, action.class, action.length
        );
    }

    Ok(())
}

fn thunks(path: &Path, limit: usize, json: bool) -> Result<()> {
    let bytes = read_file(path)?;
    let mut emulator = EmulatorBuilder::new().build()?;
    let loaded =
        emulator.load_xbe(bytes).with_context(|| format!("failed to load {}", path.display()))?;
    let start = loaded.image().header.kernel_thunk_address;
    let table = KernelThunkTable::read(emulator.memory(), start, limit)?;

    if json {
        return print_json(&table);
    }

    println!("Kernel thunk table: {}", start);
    println!("Entries:            {}", table.entries.len());
    for thunk in table.entries {
        println!("{}  ordinal {}", thunk.slot, thunk.ordinal);
    }
    Ok(())
}

fn run(path: &Path, ram_mib: usize, json: bool) -> Result<()> {
    let report = plan(path, BackendKind::DirectRewrite, ram_mib)?;
    if json {
        #[derive(Serialize)]
        struct RunReport<'a> {
            stage: &'static str,
            execution_available: bool,
            plan: &'a BootPlanReport,
        }

        return print_json(&RunReport {
            stage: "entry-block-planned",
            execution_available: false,
            plan: &report,
        });
    }

    print_plan(&report, false)?;
    println!();
    println!("Execution status: runtime emission and dispatch are not implemented.");
    Ok(())
}

fn read_file(path: &Path) -> Result<Vec<u8>> {
    fs::read(path).with_context(|| format!("failed to read {}", path.display()))
}

fn parse_u32(value: &str) -> std::result::Result<u32, String> {
    let value = value.trim();
    let parsed = if let Some(hex) = value.strip_prefix("0x").or_else(|| value.strip_prefix("0X")) {
        u32::from_str_radix(hex, 16)
    } else {
        value.parse()
    };
    parsed.map_err(|error| error.to_string())
}

fn parse_hex_bytes(input: &str) -> Result<Vec<u8>> {
    let normalized = input.replace("0x", "").replace("0X", "");
    for character in normalized.chars() {
        if !character.is_ascii_hexdigit()
            && !character.is_ascii_whitespace()
            && !matches!(character, '_' | ',' | ':' | '-')
        {
            bail!("invalid character {character:?} in the hexadecimal byte sequence");
        }
    }

    let compact =
        normalized.chars().filter(|character| character.is_ascii_hexdigit()).collect::<String>();
    if !compact.len().is_multiple_of(2) {
        bail!("the hexadecimal byte sequence contains an odd digit count");
    }

    compact
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let token = std::str::from_utf8(pair).map_err(|error| anyhow!(error))?;
            u8::from_str_radix(token, 16).map_err(|error| anyhow!(error))
        })
        .collect()
}

fn mib_to_bytes(mib: usize) -> Result<usize> {
    if mib == 0 {
        bail!("RAM size must not be zero");
    }
    mib.checked_mul(1024 * 1024).ok_or_else(|| anyhow!("RAM size overflows usize"))
}

fn print_json<T>(value: &T) -> Result<()>
where
    T: Serialize,
{
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

const fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_parser_accepts_compact_and_spaced_input() {
        assert_eq!(parse_hex_bytes("8B 01 C3").expect("hex must parse"), [0x8B, 0x01, 0xC3]);
        assert_eq!(parse_hex_bytes("0x8B01C3").expect("hex must parse"), [0x8B, 0x01, 0xC3]);
    }

    #[test]
    fn integer_parser_accepts_hexadecimal() {
        assert_eq!(parse_u32("0x1000"), Ok(4096));
    }
}
