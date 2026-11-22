use std::env;
use std::fs;
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use anyhow::Result;

use openarchive::cli::{CliArgs, Command};
use openarchive::core::inputs::expand;
use openarchive::core::{
    ArchiveFormat, CompressionOptions, Conflicts, Decision, EntryFilter, ExtractOptions, OverwriteMode, Prompter,
};
use openarchive::formats::{PasswordProblem, create_archive_from, get_reader, is_aborted, password_problem};

/// Asks on the terminal what to do with each existing file, like 7-Zip.
/// "Always", "Skip all" and "Auto rename all" answer every later question.
/// Without a terminal (scripts, pipes) it never blocks: the file is kept and
/// a warning says how to choose.
struct TerminalPrompter {
    for_all: Mutex<Option<Decision>>,
}

impl Prompter for TerminalPrompter {
    fn ask(&self, existing: &Path) -> Decision {
        let mut for_all = self.for_all.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(decision) = *for_all {
            return decision;
        }
        if !io::stdin().is_terminal() {
            eprintln!(
                "Warning: {} already exists; kept it (use -y or -aoa to overwrite, -aou to rename)",
                existing.display()
            );
            return Decision::Skip;
        }
        loop {
            eprint!(
                "\nWould you like to replace the existing file:\n  {}\n? (Y)es / (N)o / (A)lways / (S)kip all / A(u)to rename all / (Q)uit? ",
                existing.display()
            );
            let _ = io::stderr().flush();
            let mut answer = String::new();
            if io::stdin().lock().read_line(&mut answer).unwrap_or(0) == 0 {
                return Decision::Skip; // end of input
            }
            let (decision, remember) = match answer.trim().to_lowercase().as_str() {
                "y" | "yes" => (Decision::Overwrite, false),
                "n" | "no" => (Decision::Skip, false),
                "a" | "always" => (Decision::Overwrite, true),
                "s" => (Decision::Skip, true),
                "u" => (Decision::Rename, true),
                "q" | "quit" => (Decision::Abort, false),
                _ => continue,
            };
            if remember {
                *for_all = Some(decision);
            }
            return decision;
        }
    }
}

/// The entries named on the command line (wildcards allowed), or `None` for
/// the whole archive.
fn entry_filter(args: &CliArgs) -> Option<EntryFilter> {
    if args.files.is_empty() {
        return None;
    }
    Some(EntryFilter::patterns(args.files.iter().map(|f| f.to_string_lossy()), args.recurse))
}

/// `-aoa` / `-aos` / `-aou` win; otherwise `-y` overwrites; otherwise ask.
fn conflicts_for(args: &CliArgs) -> Conflicts {
    match (args.overwrite_mode, args.assume_yes) {
        (Some(mode), _) => Conflicts::new(mode),
        (None, true) => Conflicts::new(OverwriteMode::Overwrite),
        (None, false) => Conflicts::asking(Arc::new(TerminalPrompter { for_all: Mutex::new(None) })),
    }
}
use openarchive::gui::run_gui;

fn print_banner() {
    println!("\nOpenArchive (R) 0.1.0 [x64] : A High-Performance Rust Archive Engine");
    println!("Copyright (c) 2026 OpenArchive contributors\n");
}

fn print_help() {
    println!("Usage: openarchive <command> <archive_name> [<file_names>...] [<switches>...]");
    println!();
    println!("<file_names> may use wildcards: * (any characters) and ? (one character),");
    println!("e.g. a docs.7z *.txt, or x docs.7z \"notes/*.md\". Without file names,");
    println!("x, e, l and t apply to the whole archive.");
    println!();
    println!("<Commands>");
    println!("  gui : Launch OpenArchive GUI File Manager (Default when run without args)");
    println!("  a   : Add files to archive");
    println!("  e   : Extract files from archive (without directory names)");
    println!("  l   : List contents of archive");
    println!("  t   : Test integrity of archive");
    println!("  x   : eXtract files with full paths");
    println!();
    println!("<Switches>");
    println!("  -o{{dir}}      : Set Output directory");
    println!("  -p{{password}} : Set Password");
    println!("  -mx={{0..9}}   : Set compression level (0=Store, 1=Fastest, 5=Normal, 9=Ultra)");
    println!("  -md={{size}}   : Set dictionary size (e.g. -md=64m, -md=16m, -md=24)");
    println!("  -ms={{on|off}} : Enable/disable solid compression (-ms=on, -ms=off)");
    println!("  -mmt={{N|on|off}}: Set CPU thread count (-mmt=on, -mmt=1, -mmt=4)");
    println!("  -r           : Wildcards also match in subfolders (-r- : only at the top)");
    println!("  -y           : Assume Yes on all queries (overwrite existing files)");
    println!("  -ao{{a|s|u}}   : Existing files: a=overwrite all, s=skip, u=rename new files");
    println!();
    println!("Supported Formats: .7z, .zip, .tar, .tar.gz, .tar.bz2, .tar.xz");
}

/// 7-Zip's exit code for a fatal error.
const EXIT_FATAL: i32 = 2;

fn main() {
    let mut args = match CliArgs::parse_from(env::args()) {
        Ok(a) => a,
        Err(err) => {
            print_banner();
            eprintln!("Error: {}", err);
            print_help();
            process::exit(1);
        }
    };
    args.output_dir = native_separators(args.output_dir);

    if let Err(err) = run(args) {
        report(&err);
        process::exit(EXIT_FATAL);
    }
}

/// Prints a fatal error: a plain sentence for password problems (the
/// archive libraries only describe them in debug form), the error and its
/// causes otherwise.
fn report(err: &anyhow::Error) {
    match password_problem(err) {
        Some(PasswordProblem::Required) => {
            eprintln!("Error: The archive is encrypted. Password required: use -p{{password}}")
        }
        Some(PasswordProblem::Incorrect) => eprintln!("Error: Wrong password"),
        None => eprintln!("Error: {err:?}"),
    }
}

/// `/` as `\` on Windows, so paths in messages are not mixed
/// (`C:/out\dir\file`).
fn native_separators(path: PathBuf) -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(path.to_string_lossy().replace('/', "\\"))
    } else {
        path
    }
}

fn run(args: CliArgs) -> Result<()> {
    let start_time = Instant::now();

    match args.command {
        Command::Gui(initial_arc) => {
            println!("Launching OpenArchive GUI...");
            if let Err(e) = run_gui(initial_arc) {
                eprintln!("GUI Error: {}", e);
            }
        }

        Command::Help => {
            print_banner();
            print_help();
        }

        Command::Add => {
            print_banner();
            if args.files.is_empty() {
                eprintln!("Error: No files specified to add to archive.");
                process::exit(1);
            }

            let expanded = match expand(&args.files, args.recurse) {
                Ok(expanded) => expanded,
                Err(err) => {
                    eprintln!("Error: {err}");
                    process::exit(1);
                }
            };
            for name in &expanded.unmatched {
                eprintln!("WARNING: No file matches {name}");
            }
            if expanded.inputs.is_empty() {
                eprintln!("Error: Nothing to add.");
                process::exit(1);
            }

            println!("Creating archive: {}", args.archive.display());
            let options = CompressionOptions {
                level: args.compression_level,
                password: args.password,
                solid: args.solid,
                dict_size: args.dict_size,
                threads: args.threads,
            };

            print!("Compression settings: level={}", options.level);
            if let Some(ds) = options.dict_size {
                print!(", dict_size={:.1}MB", ds as f64 / (1024.0 * 1024.0));
            }
            print!(", solid={}", if options.solid { "on" } else { "off" });
            if let Some(th) = options.threads {
                print!(", threads={}", th);
            }
            println!();

            create_archive_from(&args.archive, &expanded.inputs, &options)?;

            let meta = fs::metadata(&args.archive)?;
            println!("\nArchive successfully created: {} bytes in {:.2?}", meta.len(), start_time.elapsed());
            if !expanded.unmatched.is_empty() {
                // As 7-Zip: the archive is written, but exit code 1 (warning).
                eprintln!("WARNING: {} name(s) matched no file", expanded.unmatched.len());
                process::exit(1);
            }
            println!("Everything is Ok");
        }

        Command::List => {
            print_banner();
            println!("Listing archive: {}\n", args.archive.display());

            let format = ArchiveFormat::detect(&args.archive)
                .unwrap_or(ArchiveFormat::SevenZip);

            let meta = fs::metadata(&args.archive)?;

            println!("--");
            println!("Path = {}", args.archive.display());
            println!("Type = {}", format.display_name());
            println!("Physical Size = {}", meta.len());
            println!();

            let filter = entry_filter(&args);
            let mut reader = get_reader(&args.archive, args.password)?;
            let mut entries = reader.list()?;
            if let Some(filter) = &filter {
                entries.retain(|entry| filter.matches(&entry.name));
            }

            println!("   Date      Time    Attr         Size   Compressed  Name");
            println!("------------------- ----- ------------ ------------  ------------------------");

            let mut total_size = 0u64;
            let mut file_count = 0usize;
            let mut dir_count = 0usize;

            for entry in &entries {
                let attr = if entry.is_dir { "D...." } else { "....A" };
                let date_str = "                   ";
                let comp_str = match entry.compressed_size {
                    Some(cs) => format!("{:>12}", cs),
                    None => "            ".to_string(),
                };

                println!(
                    "{} {} {:>12} {}  {}",
                    date_str,
                    attr,
                    entry.size,
                    comp_str,
                    entry.name
                );

                if entry.is_dir {
                    dir_count += 1;
                } else {
                    file_count += 1;
                    total_size += entry.size;
                }
            }

            println!("------------------- ----- ------------ ------------  ------------------------");
            println!(
                "                          {:>12}               {} files, {} folders",
                total_size, file_count, dir_count
            );
        }

        Command::ExtractFull | Command::ExtractFlat => {
            print_banner();
            let is_flat = args.command == Command::ExtractFlat;
            let verb = if is_flat { "Extracting (flat)" } else { "eXtracting" };
            println!("{} archive: {}", verb, args.archive.display());
            println!("Target directory: {}\n", args.output_dir.display());

            let mut reader = get_reader(&args.archive, args.password.clone())?;

            let filter = entry_filter(&args);

            let conflicts = Arc::new(conflicts_for(&args));
            let extract_opts = ExtractOptions {
                output_dir: args.output_dir,
                flat: is_flat,
                password: args.password,
                conflicts: conflicts.clone(),
                threads: args.threads,
                progress: None,
            };

            if let Some(th) = extract_opts.threads {
                println!("Multithreading: {} threads", th);
            }

            if let Err(err) = reader.extract(&extract_opts, filter.as_ref()) {
                if is_aborted(&err) {
                    eprintln!("\nExtraction stopped; files already extracted are kept.");
                    process::exit(255); // 7-Zip's code for "stopped by the user"
                }
                return Err(err);
            }

            println!("\nExtracted in {:.2?}", start_time.elapsed());
            if conflicts.skipped() > 0 {
                println!("Existing files kept: {}", conflicts.skipped());
            }
            if conflicts.renamed() > 0 {
                println!("Extracted under a new name: {}", conflicts.renamed());
            }
            println!("Everything is Ok");
        }

        Command::Test => {
            print_banner();
            println!("Testing archive: {}\n", args.archive.display());

            let mut reader = get_reader(&args.archive, args.password.clone())?;
            reader.test(args.password.as_deref(), entry_filter(&args).as_ref(), None)?;

            println!("\nTesting completed in {:.2?}", start_time.elapsed());
            println!("Everything is Ok");
        }
    }

    Ok(())
}
