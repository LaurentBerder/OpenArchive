use std::env;
use std::fs;
use std::process;
use std::time::Instant;
use anyhow::Result;

use openarchive::cli::{CliArgs, Command};
use openarchive::core::{ArchiveFormat, CompressionOptions, ExtractOptions};
use openarchive::formats::{create_archive, get_reader};
use openarchive::gui::run_gui;

fn print_banner() {
    println!("\nOpenArchive (R) 0.1.0 [x64] : A High-Performance Rust Archive Engine");
    println!("Copyright (c) 2026 OpenArchive contributors\n");
}

fn print_help() {
    println!("Usage: openarchive <command> <archive_name> [<file_names>...] [<switches>...]");
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
    println!("  -r           : Recurse subdirectories");
    println!("  -y           : Assume Yes on all queries");
    println!();
    println!("Supported Formats: .7z, .zip, .tar, .tar.gz, .tar.bz2");
}

fn main() -> Result<()> {
    let args = match CliArgs::parse_from(env::args()) {
        Ok(a) => a,
        Err(err) => {
            print_banner();
            eprintln!("Error: {}", err);
            print_help();
            process::exit(1);
        }
    };

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

            create_archive(&args.archive, &args.files, &options)?;

            let meta = fs::metadata(&args.archive)?;
            println!("\nArchive successfully created: {} bytes in {:.2?}", meta.len(), start_time.elapsed());
            println!("Everything is Ok");
        }

        Command::List => {
            print_banner();
            println!("Listing archive: {}\n", args.archive.display());

            let format = ArchiveFormat::from_path(&args.archive)
                .unwrap_or(ArchiveFormat::SevenZip);

            let meta = fs::metadata(&args.archive)?;

            println!("--");
            println!("Path = {}", args.archive.display());
            println!("Type = {}", format.display_name());
            println!("Physical Size = {}", meta.len());
            println!();

            let mut reader = get_reader(&args.archive, args.password)?;
            let entries = reader.list()?;

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

            let filter_strings: Vec<String> = args.files.iter().map(|p| p.to_string_lossy().to_string()).collect();
            let filter = if filter_strings.is_empty() { None } else { Some(filter_strings.as_slice()) };

            let extract_opts = ExtractOptions {
                output_dir: args.output_dir,
                flat: is_flat,
                password: args.password,
                overwrite: true,
                threads: args.threads,
                progress: None,
            };

            if let Some(th) = extract_opts.threads {
                println!("Multithreading: {} threads", th);
            }

            reader.extract(&extract_opts, filter)?;

            println!("\nExtracted in {:.2?}", start_time.elapsed());
            println!("Everything is Ok");
        }

        Command::Test => {
            print_banner();
            println!("Testing archive: {}\n", args.archive.display());

            let mut reader = get_reader(&args.archive, args.password.clone())?;
            reader.test(args.password.as_deref(), None)?;

            println!("\nTesting completed in {:.2?}", start_time.elapsed());
            println!("Everything is Ok");
        }
    }

    Ok(())
}
