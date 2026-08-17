#![forbid(unsafe_code)]

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use clap::{Parser, Subcommand, ValueEnum};
use exbawks_core::{BootPlanReport, EmulatorBuilder, EmulatorConfig};
use exbawks_cpu::{BasicBlockDecoder, DecodeConfig, format_instruction};
use exbawks_debug::{
    CoverageItem, CoverageLedger, CoverageStatus, JsonLinesTrace, Surface, SurfaceCoverage,
    TraceEventKind,
};
use exbawks_kernel::{
    ExportKind, KERNEL_ORDINALS, KernelRegistry, kernel_ordinal_info, register_startup_exports,
};
use exbawks_platform::{
    HostCapabilities, SystemMemoryInfo, probe_host_capabilities, query_system_memory_info,
};
use exbawks_types::{BackendKind, GuestVa, StopReason};
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
        /// Annotates each ordinal with its name and registry status.
        #[arg(long)]
        check_registry: bool,
    },
    /// Loads and executes one XBE until a controlled stop reason.
    Run(Box<RunArgs>),
    /// Reports the implementation burndown across emulator surfaces.
    Coverage {
        /// Restricts the report to one surface.
        #[arg(long, value_enum)]
        surface: Option<SurfaceArg>,
        /// Filters the kernel surface to one XBE's imported ordinals.
        #[arg(long)]
        xbe: Option<PathBuf>,
        /// Lists the missing elements of each reported surface.
        #[arg(long)]
        missing: bool,
    },
}

/// The arguments `run` takes.
///
/// They live in their own struct because the command carries enough of
/// them to dominate the size of every other variant.
#[derive(Debug, clap::Args)]
struct RunArgs {
    /// The XBE file path.
    path: PathBuf,
    /// The emulated physical RAM size in MiB.
    #[arg(long, default_value_t = 64)]
    ram_mib: usize,
    /// Writes JSON Lines trace events to this file.
    #[arg(long)]
    trace: Option<PathBuf>,
    /// Includes the private XBE host path in trace records.
    #[arg(long, requires = "trace")]
    trace_host_paths: bool,
    /// Restricts trace records to these event kinds.
    #[arg(long, requires = "trace", value_enum, value_delimiter = ',')]
    trace_filter: Vec<TraceFilterArg>,
    /// The maximum executed block count.
    #[arg(long, default_value_t = 1 << 20)]
    max_blocks: usize,
    /// The execution engine.
    #[arg(long, value_enum, default_value_t = EngineArg::Interpreter)]
    engine: EngineArg,
    /// Prints these guest addresses' dword values after the stop.
    #[arg(long, value_parser = parse_u32, value_delimiter = ',')]
    peek: Vec<u32>,
    /// Writes the scanned-out frame to this PNG file after the stop.
    #[arg(long)]
    screenshot: Option<PathBuf>,
    /// Prints the most-submitted graphics methods after the stop.
    #[arg(long)]
    gpu_methods: Option<usize>,
    /// Writes the most recently sampled texture to this PNG file.
    #[arg(long)]
    dump_texture: Option<PathBuf>,
    /// Captures this color surface instead of the presented one.
    #[arg(long, value_parser = parse_u32)]
    screenshot_address: Option<u32>,
    /// Prints the transform program's instruction words.
    #[arg(long)]
    gpu_program: bool,
    /// Prints the register-combiner program the last draw ran under.
    #[arg(long)]
    gpu_combiner: bool,
    /// Reports every guest write to this address, with its writer.
    #[arg(long, value_parser = parse_u32)]
    watch_write: Option<u32>,
    /// Prints the captured frame's digest, for recording a golden.
    #[arg(long)]
    frame_digest: bool,
    /// Fails the run when the captured frame's digest differs.
    #[arg(long, value_name = "DIGEST")]
    expect_frame: Option<String>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum SurfaceArg {
    Cpu,
    Kernel,
    Gpu,
}

impl From<SurfaceArg> for exbawks_debug::Surface {
    fn from(value: SurfaceArg) -> Self {
        match value {
            SurfaceArg::Cpu => Self::Cpu,
            SurfaceArg::Kernel => Self::Kernel,
            SurfaceArg::Gpu => Self::Gpu,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum EngineArg {
    /// The deterministic interpreter tier (the golden oracle).
    Interpreter,
    /// The Windows Hypervisor Platform tier (ADR 0013; native speed).
    Whp,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum BackendArg {
    Direct,
    Cranelift,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum TraceFilterArg {
    Block,
    Kernel,
    Graphics,
    Memory,
    Stop,
}

impl From<TraceFilterArg> for TraceEventKind {
    fn from(value: TraceFilterArg) -> Self {
        match value {
            TraceFilterArg::Block => Self::BlockEnter,
            TraceFilterArg::Kernel => Self::KernelCall,
            TraceFilterArg::Graphics => Self::GraphicsMethod,
            TraceFilterArg::Memory => Self::MemorySlowPath,
            TraceFilterArg::Stop => Self::Stop,
        }
    }
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
        Command::Thunks { path, limit, check_registry } => {
            thunks(&path, limit, check_registry, cli.json)
        }
        Command::Run(arguments) => {
            let RunArgs {
                path,
                ram_mib,
                trace,
                trace_host_paths,
                trace_filter,
                max_blocks,
                engine,
                peek,
                screenshot,
                gpu_methods,
                dump_texture,
                screenshot_address,
                gpu_program,
                gpu_combiner,
                watch_write,
                frame_digest,
                expect_frame,
            } = *arguments;

            let tracing = TraceOptions {
                path: trace.as_deref(),
                host_paths: trace_host_paths,
                filter: &trace_filter,
            };
            run(
                &path,
                &tracing,
                &RunOptions {
                    ram_mib,
                    max_blocks,
                    engine,
                    peek: &peek,
                    screenshot: screenshot.as_deref(),
                    gpu_methods,
                    dump_texture: dump_texture.as_deref(),
                    screenshot_address,
                    gpu_program,
                    gpu_combiner,
                    watch_write,
                    frame_digest,
                    expect_frame: expect_frame.as_deref(),
                    json: cli.json,
                },
            )
        }
        Command::Coverage { surface, xbe, missing } => {
            coverage(surface.map(Into::into), xbe.as_deref(), missing, cli.json)
        }
    }
}

fn initialize_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt().with_env_filter(filter).try_init();
}

#[derive(Debug, Serialize)]
struct DoctorReport {
    #[serde(flatten)]
    capabilities: HostCapabilities,
    memory: Option<SystemMemoryInfo>,
    whp: exbawks_whp::WhpAvailability,
}

fn doctor(json: bool) -> Result<()> {
    let report = DoctorReport {
        capabilities: probe_host_capabilities(),
        memory: query_system_memory_info().ok(),
        whp: exbawks_whp::probe_whp(),
    };

    if json {
        return print_json(&report);
    }

    let capabilities = &report.capabilities;
    println!("Operating system:       {}", capabilities.operating_system);
    println!("Architecture:           {}", capabilities.architecture);
    println!("Windows x86-64 target:  {}", yes_no(capabilities.supported_runtime_target));
    println!("Placeholder views:      {}", yes_no(capabilities.placeholder_views));
    println!("FSGSBASE available:     {}", yes_no(capabilities.fsgsbase));
    println!("WHP library present:    {}", yes_no(report.whp.library_present));
    println!("WHP hypervisor present: {}", yes_no(report.whp.hypervisor_present));
    println!("WHP execution tier:     {}", yes_no(report.whp.usable()));
    match report.memory {
        Some(memory) => {
            println!("Host page size:         {} bytes", memory.page_size);
            println!("Allocation granularity: {} bytes", memory.allocation_granularity);
        }
        None => {
            println!("Memory geometry:        unavailable on this host");
        }
    }
    if !capabilities.supported_runtime_target {
        println!("Runtime status:         unsupported for execution; logic tools remain available");
    }
    Ok(())
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
    if let Some(translated) = report.translated_instructions {
        println!(
            "Translated:          {translated} of {} instruction(s)",
            report.decoded_instructions
        );
    }
    if let Some(static_exit) = &report.static_exit {
        println!("Static exit:         {static_exit}");
    }
    println!("Block stop:          {}", report.block_stop);

    for action in &report.actions {
        println!(
            "{}  {:<32} {:<16} {} byte(s)",
            action.address, action.instruction, action.class, action.length
        );
    }

    Ok(())
}

fn thunks(path: &Path, limit: usize, check_registry: bool, json: bool) -> Result<()> {
    let bytes = read_file(path)?;
    let config = EmulatorConfig { max_kernel_thunks: limit, ..EmulatorConfig::default() };
    let mut emulator = EmulatorBuilder::new().config(config).build()?;
    let loaded =
        emulator.load_xbe(bytes).with_context(|| format!("failed to load {}", path.display()))?;
    let start = loaded.image().header.kernel_thunk_address;
    let table = loaded.kernel_thunks();

    if check_registry {
        let report = annotate_thunks(start, &table.entries, emulator.kernel());
        if json {
            return print_json(&report);
        }
        print_thunk_registry(&report);
        return Ok(());
    }

    if json {
        return print_json(table);
    }

    println!("Kernel thunk table: {}", start);
    println!("Entries:            {}", table.entries.len());
    for thunk in &table.entries {
        println!("{}  ordinal {}", thunk.slot, thunk.ordinal);
    }
    Ok(())
}

/// One annotated kernel import for registry triage.
#[derive(Debug, Serialize)]
struct ThunkRegistryEntry {
    slot: GuestVa,
    ordinal: u16,
    name: String,
    kind: &'static str,
    status: &'static str,
}

/// A registry-coverage summary for one thunk table.
#[derive(Debug, Serialize)]
struct ThunkRegistryReport {
    start: GuestVa,
    implemented: usize,
    stubs: usize,
    missing: usize,
    entries: Vec<ThunkRegistryEntry>,
}

fn annotate_thunks(
    start: GuestVa,
    entries: &[exbawks_core::KernelThunk],
    registry: &exbawks_kernel::KernelRegistry,
) -> ThunkRegistryReport {
    let mut report =
        ThunkRegistryReport { start, implemented: 0, stubs: 0, missing: 0, entries: Vec::new() };

    for thunk in entries {
        let info = exbawks_kernel::kernel_ordinal_info(thunk.ordinal);
        let name =
            info.map_or_else(|| format!("ordinal-{}", thunk.ordinal), |info| info.name.to_owned());
        let kind = match info.map(|info| info.kind) {
            Some(exbawks_kernel::ExportKind::Function) => "function",
            Some(exbawks_kernel::ExportKind::Data) => "data",
            None => "unknown",
        };
        let status = match registry.get(thunk.ordinal) {
            Some(export) if export.is_stub() => {
                report.stubs += 1;
                "stub"
            }
            Some(_) => {
                report.implemented += 1;
                "implemented"
            }
            None => {
                report.missing += 1;
                "missing"
            }
        };
        report.entries.push(ThunkRegistryEntry {
            slot: thunk.slot,
            ordinal: thunk.ordinal,
            name,
            kind,
            status,
        });
    }

    report
}

fn print_thunk_registry(report: &ThunkRegistryReport) {
    println!("Kernel thunk table: {}", report.start);
    println!("Entries:            {}", report.entries.len());
    println!("Implemented:        {}", report.implemented);
    println!("Stubs:              {}", report.stubs);
    println!("Missing:            {}", report.missing);
    for entry in &report.entries {
        println!(
            "{}  ordinal {:>3}  {:<32} {:<8} {}",
            entry.slot, entry.ordinal, entry.name, entry.kind, entry.status
        );
    }
}

/// Trace-output options for one run.
struct TraceOptions<'a> {
    /// The JSON Lines output path, when tracing is on.
    path: Option<&'a Path>,
    /// Whether records may carry private host paths.
    host_paths: bool,
    /// The event-kind filter; empty records everything.
    filter: &'a [TraceFilterArg],
}

/// One `run` invocation's options beyond the image itself.
struct RunOptions<'a> {
    /// The emulated physical RAM size in MiB.
    ram_mib: usize,
    /// The maximum executed block count.
    max_blocks: usize,
    /// The execution engine.
    engine: EngineArg,
    /// Guest addresses whose dwords print after the stop.
    peek: &'a [u32],
    /// Where the captured frame is written, when asked for.
    screenshot: Option<&'a Path>,
    /// How many graphics methods to report, when asked for.
    gpu_methods: Option<usize>,
    /// Where the most recently sampled texture is written, when asked for.
    dump_texture: Option<&'a Path>,
    /// A specific color surface to capture instead of the presented one.
    screenshot_address: Option<u32>,
    /// Whether to print the transform program's instruction words.
    gpu_program: bool,
    /// Whether to print the register-combiner program.
    gpu_combiner: bool,
    /// A guest address whose writers should be reported.
    watch_write: Option<u32>,
    /// Whether to print the captured frame's digest.
    frame_digest: bool,
    /// The digest the captured frame must match, when one is expected.
    expect_frame: Option<&'a str>,
    /// Whether the report prints as JSON.
    json: bool,
}

fn run(path: &Path, tracing: &TraceOptions<'_>, options: &RunOptions<'_>) -> Result<()> {
    let RunOptions {
        ram_mib,
        max_blocks,
        engine,
        peek,
        screenshot,
        gpu_methods,
        dump_texture,
        screenshot_address,
        gpu_program,
        gpu_combiner,
        watch_write,
        frame_digest,
        expect_frame,
        json,
    } = *options;
    let bytes = read_file(path)?;
    let config = EmulatorConfig {
        physical_memory_bytes: mib_to_bytes(ram_mib)?,
        ..EmulatorConfig::default()
    };
    let mut builder = EmulatorBuilder::new().config(config);
    if let Some(trace_path) = tracing.path {
        let file = fs::File::create(trace_path)
            .with_context(|| format!("failed to create {}", trace_path.display()))?;
        let mut sink = JsonLinesTrace::new(std::io::BufWriter::new(file));
        if tracing.host_paths {
            sink = sink.with_host_path(path.display().to_string());
        }
        if !tracing.filter.is_empty() {
            sink = sink.with_event_filter(tracing.filter.iter().copied().map(TraceEventKind::from));
        }
        builder = builder.trace(std::sync::Arc::new(sink));
    }
    let mut emulator = builder.build()?;
    // Mount the image's own directory as the read-only game disc (ADR 0014),
    // so the guest's file exports read the files shipped alongside the XBE.
    if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
        emulator.set_disc_root(parent.to_path_buf());
    }
    // Mount a persistent per-title directory as the writable hard disk
    // (ADR 0016), so the title can create its save directories and files.
    match hdd_root_for(&bytes) {
        Ok(hdd) => emulator.set_hdd_root(hdd),
        Err(error) => eprintln!("warning: no writable hard-disk mount: {error:#}"),
    }
    if let Some(address) = watch_write {
        emulator.watch_writes(GuestVa(address));
    }
    emulator.load_xbe(bytes).with_context(|| format!("failed to load {}", path.display()))?;
    let stop = match engine {
        EngineArg::Interpreter => emulator.run(max_blocks)?,
        #[cfg(all(windows, target_arch = "x86_64"))]
        EngineArg::Whp => emulator.run_whp(max_blocks)?,
        #[cfg(not(all(windows, target_arch = "x86_64")))]
        EngineArg::Whp => bail!("the WHP engine requires Windows x86-64"),
    };

    if json {
        #[derive(Serialize)]
        struct RunReport {
            stop: StopReason,
            eip: GuestVa,
            gpr: [u32; 8],
        }

        return print_json(&RunReport {
            stop,
            eip: GuestVa(emulator.cpu().eip),
            gpr: emulator.cpu().gpr,
        });
    }

    // TEMP TLS investigation.
    if std::env::var("EXBAWKS_TLS_PROBE").is_ok() {
        use exbawks_memory::GuestMemory;
        let mem = emulator.memory();
        let read = |addr: u32| mem.read_u32(GuestVa(addr)).map(|v| format!("{v:#010x}"));
        eprintln!("_tls_index [0x61E828] = {:?}", read(0x0061_E828));
        for kpcr in [0x8010_1000u32, 0x8022_4000] {
            eprintln!(
                "kpcr {kpcr:#x}: fs[4]={:?} array[0]={:?} array[1]={:?}",
                read(kpcr + 4),
                read(kpcr + 0x40),
                read(kpcr + 0x44)
            );
        }
    }

    let gpr = emulator.cpu().gpr;
    println!("Stop reason:  {stop:?}{}", stop_reason_note(&stop));
    println!("Final EIP:    {}", GuestVa(emulator.cpu().eip));
    println!("Final EAX:    0x{:08X}", gpr[0]);
    println!("Final ECX:    0x{:08X}", gpr[1]);
    println!("Final EDX:    0x{:08X}", gpr[2]);
    println!("Final EBX:    0x{:08X}", gpr[3]);
    println!("Final ESP:    0x{:08X}", gpr[4]);
    println!("Final EBP:    0x{:08X}", gpr[5]);
    println!("Final ESI:    0x{:08X}", gpr[6]);
    println!("Final EDI:    0x{:08X}", gpr[7]);

    if let Some(limit) = gpu_methods {
        let histogram = emulator.gpu_method_histogram(limit);
        println!(
            "
Graphics methods (object, method, submissions):"
        );
        for (handle, method, count) in histogram {
            println!("  {handle:#010x}  {method:#06x}  {count}");
        }
        println!();
    }

    if gpu_methods.is_some() {
        use exbawks_memory::GuestMemory;

        // A surface's pixel count says where drawing landed; its share of
        // non-black pixels says whether anything survived there.
        println!("Busiest color surfaces (address, pixels drawn, non-black sample):");
        for (base, pixels) in emulator.gpu_busiest_targets(8) {
            let mut sampled = 0_u32;
            let mut lit = 0_u32;
            for index in (0..640 * 480).step_by(16) {
                let address = 0x8000_0000 | base.wrapping_add(index * 4);
                if let Ok(value) = emulator.memory().read_u32(GuestVa(address)) {
                    sampled += 1;
                    if value & 0x00FF_FFFF != 0 {
                        lit += 1;
                    }
                }
            }
            let percent = (lit * 100).checked_div(sampled).unwrap_or(0);
            println!("  {}  {pixels}  {percent}%", GuestVa(base));
        }
        println!();
    }

    if gpu_program {
        let program = emulator.gpu_transform_program();
        println!("Transform program ({} slots):", program.len());
        for (slot, words) in program.iter().enumerate().take(64) {
            println!(
                "  {slot:3}: {:08x} {:08x} {:08x} {:08x}",
                words[0], words[1], words[2], words[3]
            );
        }
        println!();
    }

    if gpu_combiner {
        let combiner = emulator.gpu_combiner();
        println!(
            "Register combiners ({} stages, control {:08x}):",
            combiner.active, combiner.control
        );
        for stage in 0..combiner.stages.len() {
            let programmed = combiner.stages[stage];
            println!(
                "  {stage}: color in {:08x} out {:08x}  alpha in {:08x} out {:08x}",
                programmed.color_inputs,
                programmed.color_outputs,
                programmed.alpha_inputs,
                programmed.alpha_outputs
            );
        }
        println!("  final: {:08x} {:08x}", combiner.final_first, combiner.final_second);
        println!();
    }

    if let Some(path) = dump_texture {
        match emulator.capture_last_texture() {
            Some(frame) => {
                let png = exbawks_debug::encode_rgba(frame.width, frame.height, &frame.pixels);
                fs::write(path, &png)
                    .with_context(|| format!("failed to write {}", path.display()))?;
                println!("Texture:      {}x{} -> {}", frame.width, frame.height, path.display());
            }
            None => eprintln!("warning: no texture was sampled"),
        }
    }

    if let Some(path) = screenshot {
        let captured = match screenshot_address {
            Some(address) => emulator.capture_surface_at(address),
            None => emulator.capture_frame(),
        };
        match captured {
            Ok(frame) => {
                let png = exbawks_debug::encode_rgba(frame.width, frame.height, &frame.pixels);
                fs::write(path, &png)
                    .with_context(|| format!("failed to write {}", path.display()))?;
                println!(
                    "Screenshot:   {}x{} from {} -> {}",
                    frame.width,
                    frame.height,
                    GuestVa(frame.frame_buffer),
                    path.display()
                );
            }
            Err(error) => eprintln!("warning: no frame captured: {error}"),
        }
    }

    if !peek.is_empty() {
        use exbawks_memory::GuestMemory;
        for &address in peek {
            match emulator.memory().read_u32(GuestVa(address)) {
                Ok(value) => println!("Peek [{}]: 0x{value:08X}", GuestVa(address)),
                Err(_) => println!("Peek [{}]: <unmapped>", GuestVa(address)),
            }
        }
    }

    if frame_digest || expect_frame.is_some() {
        let frame = match screenshot_address {
            Some(address) => emulator.capture_surface_at(address),
            None => emulator.capture_frame(),
        }
        .map_err(|error| anyhow!("no frame to digest: {error}"))?;
        let digest = exbawks_debug::frame_digest(frame.width, frame.height, &frame.pixels);
        println!("Frame digest: {digest}");
        if let Some(expected) = expect_frame
            && expected != digest
        {
            bail!("the frame digest is {digest}, and {expected} was expected");
        }
    }

    if let Some(diagnosis) = diagnose_stop(&emulator, &stop) {
        println!("\n{diagnosis}");
    }
    Ok(())
}

/// Renders an ariadne diagnosis of a run's stop site, when the stop names a
/// coverage gap at a decodable guest address.
fn diagnose_stop(emulator: &exbawks_core::Emulator, stop: &StopReason) -> Option<String> {
    let (title, label, note) = match stop {
        StopReason::MissingKernelExport { ordinal }
        | StopReason::UnimplementedKernelExport { ordinal } => {
            let name = kernel_ordinal_info(*ordinal)
                .map_or_else(|| format!("ordinal-{ordinal}"), |info| info.name.to_owned());
            let state = match stop {
                StopReason::UnimplementedKernelExport { .. } => "stub, no semantics",
                _ => "not registered",
            };
            (
                "kernel HLE surface reached an unimplemented export".to_owned(),
                format!("calls {name} (ordinal {ordinal}) — {state}"),
                Some(
                    "burndown: run `exbawks coverage --surface kernel --missing` for the gap list"
                        .to_owned(),
                ),
            )
        }
        StopReason::UnsupportedInstruction { .. } => (
            "interpreter oracle reached an unimplemented instruction".to_owned(),
            "not in the tier-0 instruction set".to_owned(),
            None,
        ),
        StopReason::GuestFault { address } => (
            "guest raised a fault the runtime cannot deliver".to_owned(),
            format!("faulting access to {address}"),
            None,
        ),
        _ => return None,
    };

    let eip = emulator.cpu().eip;
    let mut bytes = [0_u8; 32];
    let read = read_guest_prefix(emulator.memory(), GuestVa(eip), &mut bytes)?;
    let block = BasicBlockDecoder::new(DecodeConfig { max_instructions: 6, max_bytes: read })
        .decode(GuestVa(eip), &bytes[..read])
        .ok()?;
    if block.instructions.is_empty() {
        return None;
    }
    let lines: Vec<String> = block
        .instructions
        .iter()
        .map(|instruction| {
            format!("{}  {}", GuestVa(instruction.ip() as u32), format_instruction(instruction))
        })
        .collect();
    Some(exbawks_debug::render_site(&title, &lines, 0, &label, note.as_deref()))
}

/// Reads as many mapped bytes at `address` as fit before an unmapped page.
fn read_guest_prefix(
    memory: &exbawks_memory::SoftwareAddressSpace,
    address: GuestVa,
    buffer: &mut [u8],
) -> Option<usize> {
    use exbawks_memory::GuestMemory;
    for len in (1..=buffer.len()).rev() {
        if memory.fetch(address, &mut buffer[..len]).is_ok() {
            return Some(len);
        }
    }
    None
}

/// Names the export behind kernel-related stop reasons.
/// Creates and returns the per-title writable hard-disk directory
/// (ADR 0016): `%LOCALAPPDATA%\exbawks\hdd\<title-id>\`, falling back to the
/// system temp directory when `LOCALAPPDATA` is unset.
fn hdd_root_for(xbe_bytes: &[u8]) -> Result<PathBuf> {
    // The certificate's dwTitleId lives at certificate+8; the header region
    // maps 1:1 from the file start at the base address.
    let read_u32 = |offset: usize| -> Option<u32> {
        let slice = xbe_bytes.get(offset..offset + 4)?;
        Some(u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
    };
    let base = read_u32(0x104).context("XBE too short for a base address")?;
    let certificate = read_u32(0x118).context("XBE too short for a certificate address")?;
    let title_id = certificate
        .checked_sub(base)
        .and_then(|offset| read_u32(offset as usize + 8))
        .context("certificate lies outside the image bytes")?;

    let base_dir =
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let hdd = base_dir.join("exbawks").join("hdd").join(format!("{title_id:08X}"));
    fs::create_dir_all(&hdd)
        .with_context(|| format!("failed to create hard-disk directory {}", hdd.display()))?;
    Ok(hdd)
}

fn stop_reason_note(stop: &StopReason) -> String {
    match stop {
        StopReason::MissingKernelExport { ordinal }
        | StopReason::UnimplementedKernelExport { ordinal } => {
            exbawks_kernel::kernel_ordinal_info(*ordinal)
                .map(|info| format!(" ({})", info.name))
                .unwrap_or_default()
        }
        StopReason::Reboot { .. } => {
            " (the title rebooted; a self-relaunch loop or a dashboard return)".to_owned()
        }
        _ => String::new(),
    }
}

fn coverage(only: Option<Surface>, xbe: Option<&Path>, missing: bool, json: bool) -> Result<()> {
    let imports = match xbe {
        Some(path) => Some(xbe_import_ordinals(path)?),
        None => None,
    };
    let want = |surface: Surface| only.is_none_or(|selected| selected == surface);

    let mut ledger = CoverageLedger::default();
    if want(Surface::Cpu) {
        ledger.push(cpu_surface());
    }
    if want(Surface::Kernel) {
        ledger.push(kernel_surface(imports.as_deref()));
    }
    if want(Surface::Gpu) {
        ledger.push(gpu_surface());
    }

    if json {
        return print_json(&CoverageJson::from_ledger(&ledger));
    }
    print_coverage(&ledger, missing);
    Ok(())
}

/// Reads the imported kernel ordinals of one XBE.
fn xbe_import_ordinals(path: &Path) -> Result<Vec<u16>> {
    let bytes = read_file(path)?;
    let mut emulator = EmulatorBuilder::new().build()?;
    let loaded =
        emulator.load_xbe(bytes).with_context(|| format!("failed to load {}", path.display()))?;
    let mut ordinals: Vec<u16> =
        loaded.kernel_thunks().entries.iter().map(|thunk| thunk.ordinal).collect();
    ordinals.sort_unstable();
    ordinals.dedup();
    Ok(ordinals)
}

/// Builds the kernel HLE ordinal coverage, optionally filtered to imports.
fn kernel_surface(imports: Option<&[u16]>) -> SurfaceCoverage {
    let registry = KernelRegistry::new();
    let _ = register_startup_exports(&registry);
    let ordinals: Vec<u16> = match imports {
        Some(list) => list.to_vec(),
        None => KERNEL_ORDINALS.iter().map(|entry| entry.ordinal).collect(),
    };

    let items = ordinals
        .into_iter()
        .map(|ordinal| {
            let info = kernel_ordinal_info(ordinal);
            let name =
                info.map_or_else(|| format!("ordinal-{ordinal}"), |info| info.name.to_owned());
            let status = match registry.get(ordinal) {
                Some(export) if export.is_stub() => CoverageStatus::Stub,
                Some(_) => CoverageStatus::Implemented,
                None => CoverageStatus::Missing,
            };
            let note = info.map(|info| match info.kind {
                ExportKind::Function => "function".to_owned(),
                ExportKind::Data => "data".to_owned(),
            });
            CoverageItem { id: u32::from(ordinal), name, status, note }
        })
        .collect();
    SurfaceCoverage::new(Surface::Kernel, items)
}

/// Builds the interpreter oracle's instruction-family coverage.
///
/// Under the WHP execution tier the host CPU runs these natively; this
/// surface tracks the deterministic oracle tier that produces goldens.
fn cpu_surface() -> SurfaceCoverage {
    use CoverageStatus::{Implemented, Missing};
    const FAMILIES: &[(&str, CoverageStatus)] = &[
        ("mov / movzx / movsx / lea", Implemented),
        ("alu add/adc/sub/sbb/and/or/xor/cmp/test", Implemented),
        ("inc / dec / neg / not", Implemented),
        ("shift / rotate (incl. shld/shrd)", Implemented),
        ("mul / imul / div / idiv", Implemented),
        ("bt family / bsf / bsr / bswap", Implemented),
        ("setcc / cmovcc", Implemented),
        ("xchg / xadd / cmpxchg / cmpxchg8b", Implemented),
        ("string ops with rep prefixes", Implemented),
        ("jmp / jcc / call / ret / loop / jecxz", Implemented),
        ("push / pop / pushad / popad / pushfd / popfd / leave", Implemented),
        ("cbw / cwde / cwd / cdq / flag ops", Implemented),
        ("cpuid / rdtsc", Implemented),
        ("x87 fpu", Missing),
        ("mmx", Missing),
        ("sse1", Missing),
    ];
    let items = FAMILIES
        .iter()
        .enumerate()
        .map(|(index, (name, status))| CoverageItem {
            id: index as u32,
            name: (*name).to_owned(),
            status: *status,
            note: None,
        })
        .collect();
    SurfaceCoverage::new(Surface::Cpu, items)
}

/// Builds the NV2A graphics method coverage (scaffold; none implemented).
fn gpu_surface() -> SurfaceCoverage {
    const METHODS: &[&str] = &[
        "SET_OBJECT",
        "pushbuffer FIFO kick",
        "CLEAR",
        "BEGIN_END primitives",
        "inline vertex arrays",
        "texture and format state",
        "blend and alpha state",
        "viewport and transform",
        "PCRTC present / flip",
    ];
    let items = METHODS
        .iter()
        .enumerate()
        .map(|(index, name)| CoverageItem {
            id: index as u32,
            name: (*name).to_owned(),
            status: CoverageStatus::Missing,
            note: None,
        })
        .collect();
    SurfaceCoverage::new(Surface::Gpu, items)
}

fn print_coverage(ledger: &CoverageLedger, missing: bool) {
    println!(
        "{:<8} {:>11} {:>5} {:>7} {:>6} {:>4}",
        "Surface", "Implemented", "Stub", "Missing", "Total", "%"
    );
    for surface in &ledger.surfaces {
        println!(
            "{:<8} {:>11} {:>5} {:>7} {:>6} {:>3}%",
            surface.surface.as_str(),
            surface.count(CoverageStatus::Implemented),
            surface.count(CoverageStatus::Stub),
            surface.count(CoverageStatus::Missing),
            surface.total(),
            surface.percent_implemented(),
        );
    }

    if missing {
        for surface in &ledger.surfaces {
            let gaps: Vec<&CoverageItem> = surface.missing().collect();
            if gaps.is_empty() {
                continue;
            }
            println!("\n{} missing ({}):", surface.surface, gaps.len());
            for item in gaps {
                match &item.note {
                    Some(note) => println!("  {:>4}  {:<32} {}", item.id, item.name, note),
                    None => println!("  {:>4}  {}", item.id, item.name),
                }
            }
        }
    }
}

#[derive(Debug, Serialize)]
struct CoverageJson {
    surfaces: Vec<SurfaceJson>,
}

#[derive(Debug, Serialize)]
struct SurfaceJson {
    surface: String,
    implemented: usize,
    stub: usize,
    missing: usize,
    total: usize,
    percent_implemented: u32,
    items: Vec<ItemJson>,
}

#[derive(Debug, Serialize)]
struct ItemJson {
    id: u32,
    name: String,
    status: String,
    note: Option<String>,
}

impl CoverageJson {
    fn from_ledger(ledger: &CoverageLedger) -> Self {
        let surfaces = ledger
            .surfaces
            .iter()
            .map(|surface| SurfaceJson {
                surface: surface.surface.as_str().to_owned(),
                implemented: surface.count(CoverageStatus::Implemented),
                stub: surface.count(CoverageStatus::Stub),
                missing: surface.count(CoverageStatus::Missing),
                total: surface.total(),
                percent_implemented: surface.percent_implemented(),
                items: surface
                    .items
                    .iter()
                    .map(|item| ItemJson {
                        id: item.id,
                        name: item.name.clone(),
                        status: item.status.as_str().to_owned(),
                        note: item.note.clone(),
                    })
                    .collect(),
            })
            .collect();
        Self { surfaces }
    }
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

    #[test]
    fn thunk_annotation_reports_all_three_registry_states() {
        use exbawks_core::KernelThunk;
        use exbawks_kernel::{DbgPrint, KernelRegistry, StubExport, ordinal};

        let registry = KernelRegistry::new();
        registry.register(DbgPrint).expect("DbgPrint must register");
        registry
            .register(StubExport::new(ordinal::NT_CREATE_FILE, "NtCreateFile"))
            .expect("stub must register");

        let entries = [
            KernelThunk { slot: GuestVa(0x1000), ordinal: ordinal::DBG_PRINT },
            KernelThunk { slot: GuestVa(0x1004), ordinal: ordinal::NT_CREATE_FILE },
            KernelThunk { slot: GuestVa(0x1008), ordinal: 156 },
        ];
        let report = annotate_thunks(GuestVa(0x1000), &entries, &registry);

        assert_eq!(report.implemented, 1);
        assert_eq!(report.stubs, 1);
        assert_eq!(report.missing, 1);
        assert_eq!(report.entries[0].name, "DbgPrint");
        assert_eq!(report.entries[0].status, "implemented");
        assert_eq!(report.entries[1].status, "stub");
        assert_eq!(report.entries[2].name, "KeTickCount");
        assert_eq!(report.entries[2].kind, "data");
        assert_eq!(report.entries[2].status, "missing");
    }
}
