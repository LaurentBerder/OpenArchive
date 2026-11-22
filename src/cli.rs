use std::path::PathBuf;

use crate::core::OverwriteMode;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Add,
    ExtractFull, // x
    ExtractFlat, // e
    List,        // l
    Test,        // t
    Gui(Option<PathBuf>),
    Help,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct CliArgs {
    pub command: Command,
    pub archive: PathBuf,
    pub files: Vec<PathBuf>,
    pub output_dir: PathBuf,
    pub password: Option<String>,
    pub compression_level: u8,
    pub solid: bool,
    pub dict_size: Option<u32>,
    pub threads: Option<usize>,
    pub recurse: bool,
    pub assume_yes: bool,
    /// `-aoa` / `-aos` / `-aou`; `None` means ask (or overwrite with `-y`).
    pub overwrite_mode: Option<OverwriteMode>,
}

pub fn parse_dict_size(val: &str) -> Option<u32> {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;
    const MAX_U32: u64 = u32::MAX as u64;

    let s = val.trim().to_lowercase();
    if s.is_empty() {
        return None;
    }

    // 7-Zip's largest dictionary is 4 GB. `dict_size` is a u32, so represent
    // exactly 4 GB with u32::MAX; LZMA2 clamps it to the largest encodable
    // dictionary (4 GB - 16 bytes).
    let clamp_4gb = |bytes: u64| -> Option<u32> {
        if bytes <= MAX_U32 {
            Some(bytes as u32)
        } else if bytes == 4 * GB {
            Some(u32::MAX)
        } else {
            None
        }
    };

    if let Some(num) = s.strip_suffix("gb").or_else(|| s.strip_suffix('g')) {
        clamp_4gb(num.trim().parse::<u64>().ok()?.checked_mul(GB)?)
    } else if let Some(num) = s.strip_suffix("mb").or_else(|| s.strip_suffix('m')) {
        clamp_4gb(num.trim().parse::<u64>().ok()?.checked_mul(MB)?)
    } else if let Some(num) = s.strip_suffix("kb").or_else(|| s.strip_suffix('k')) {
        clamp_4gb(num.trim().parse::<u64>().ok()?.checked_mul(KB)?)
    } else if let Some(num) = s.strip_suffix('b') {
        clamp_4gb(num.trim().parse::<u64>().ok()?)
    } else if let Ok(n) = s.parse::<u64>() {
        if n <= 32 {
            // 7-Zip convention: bit exponents, so 24 means 2^24 bytes.
            if n == 32 {
                Some(u32::MAX)
            } else {
                1u32.checked_shl(n as u32)
            }
        } else {
            clamp_4gb(n)
        }
    } else {
        None
    }
}

impl CliArgs {
    pub fn parse_from<I, T>(args: I) -> Result<Self, String>
    where
        I: IntoIterator<Item = T>,
        T: Into<String>,
    {
        let raw: Vec<String> = args.into_iter().map(|s| s.into()).collect();
        if raw.len() <= 1 {
            return Ok(Self {
                command: Command::Gui(None),
                archive: PathBuf::new(),
                files: Vec::new(),
                output_dir: PathBuf::from("."),
                password: None,
                compression_level: 5,
                solid: true,
                dict_size: None,
                threads: None,
                recurse: false,
                assume_yes: false,
                overwrite_mode: None,
            });
        }

        let mut command_str = String::new();
        let mut positional = Vec::new();
        let mut output_dir = PathBuf::from(".");
        let mut password = None;
        let mut compression_level = 5;
        let mut solid = true;
        let mut dict_size = None;
        let mut threads = None;
        let mut recurse = false;
        let mut assume_yes = false;
        let mut overwrite_mode = None;

        let mut i = 1;
        while i < raw.len() {
            let arg = &raw[i];

            if arg == "-h" || arg == "--help" || arg == "help" {
                return Ok(Self::default_help());
            }

            // Switches
            if arg.starts_with("-o") {
                let val = arg[2..].trim_start_matches('=').to_string();
                if !val.is_empty() {
                    output_dir = PathBuf::from(val);
                } else if i + 1 < raw.len() {
                    i += 1;
                    output_dir = PathBuf::from(&raw[i]);
                }
            } else if arg.starts_with("-p") {
                let val = arg[2..].trim_start_matches('=').to_string();
                if !val.is_empty() {
                    password = Some(val);
                } else if i + 1 < raw.len() {
                    i += 1;
                    password = Some(raw[i].clone());
                }
            } else if arg.starts_with("-mx") {
                let val = arg[3..].trim_start_matches('=').trim().to_string();
                if !val.is_empty() {
                    if let Ok(lvl) = val.parse::<u8>() {
                        compression_level = lvl.clamp(0, 9);
                    }
                } else if i + 1 < raw.len() {
                    if let Ok(lvl) = raw[i + 1].trim().parse::<u8>() {
                        i += 1;
                        compression_level = lvl.clamp(0, 9);
                    }
                }
            } else if arg.starts_with("-md") {
                let val = arg[3..].trim_start_matches('=').trim().to_string();
                if !val.is_empty() {
                    dict_size = parse_dict_size(&val);
                } else if i + 1 < raw.len() {
                    i += 1;
                    dict_size = parse_dict_size(&raw[i]);
                }
            } else if arg.starts_with("-ms") {
                let val = arg[3..].trim_start_matches('=').trim().to_lowercase();
                if val.is_empty() || val == "on" || val == "+" || val == "yes" {
                    solid = true;
                } else if val == "off" || val == "-" || val == "no" || val == "e" {
                    solid = false;
                } else if i + 1 < raw.len() {
                    let next_val = raw[i + 1].trim_start_matches('=').trim().to_lowercase();
                    if next_val == "on" || next_val == "+" || next_val == "yes" {
                        i += 1;
                        solid = true;
                    } else if next_val == "off"
                        || next_val == "-"
                        || next_val == "no"
                        || next_val == "e"
                    {
                        i += 1;
                        solid = false;
                    }
                }
            } else if arg.starts_with("-mmt") {
                let val = arg[4..].trim_start_matches('=').trim().to_lowercase();
                if val.is_empty() || val == "on" || val == "+" || val == "yes" {
                    let count = crate::core::available_threads();
                    threads = Some(count);
                } else if val == "off" || val == "-" || val == "no" {
                    threads = Some(1);
                } else if let Ok(n) = val.parse::<usize>() {
                    threads = Some(n.max(1));
                } else if i + 1 < raw.len() {
                    let next_val = raw[i + 1].trim_start_matches('=').trim().to_lowercase();
                    if next_val == "on" || next_val == "+" || next_val == "yes" {
                        i += 1;
                        let count = crate::core::available_threads();
                        threads = Some(count);
                    } else if next_val == "off" || next_val == "-" || next_val == "no" {
                        i += 1;
                        threads = Some(1);
                    } else if let Ok(n) = next_val.parse::<usize>() {
                        i += 1;
                        threads = Some(n.max(1));
                    }
                }
            } else if arg == "-r" || arg == "-r-" {
                recurse = arg == "-r";
            } else if arg == "-y" {
                assume_yes = true;
            } else if let Some(mode) = arg.strip_prefix("-ao") {
                overwrite_mode = Some(match mode {
                    "a" => OverwriteMode::Overwrite,
                    "s" => OverwriteMode::Skip,
                    "u" => OverwriteMode::Rename,
                    _ => {
                        return Err(format!(
                            "unsupported switch {arg}: use -aoa (overwrite), -aos (skip) or -aou (rename new files)"
                        ));
                    }
                });
            } else if arg.starts_with('-') {
                // Ignore other switches gracefully or handle options
            } else if command_str.is_empty() {
                command_str = arg.to_lowercase();
            } else {
                positional.push(arg.clone());
            }
            i += 1;
        }

        let command = match command_str.as_str() {
            "gui" => Command::Gui(positional.first().map(PathBuf::from)),
            "a" | "add" => Command::Add,
            "x" | "extract" => Command::ExtractFull,
            "e" => Command::ExtractFlat,
            "l" | "list" => Command::List,
            "t" | "test" => Command::Test,
            _ => {
                let candidate = PathBuf::from(&command_str);
                if candidate.exists()
                    || command_str.ends_with(".7z")
                    || command_str.ends_with(".zip")
                {
                    Command::Gui(Some(candidate))
                } else {
                    return Err(format!(
                        "Unknown command: '{}'. Use 'a', 'x', 'e', 'l', 't', or 'gui'.",
                        command_str
                    ));
                }
            }
        };

        if matches!(command, Command::Gui(_)) {
            let arc = positional.first().map(PathBuf::from).unwrap_or_default();
            return Ok(Self {
                command,
                archive: arc,
                files: Vec::new(),
                output_dir,
                password,
                compression_level,
                solid,
                dict_size,
                threads,
                recurse,
                assume_yes,
                overwrite_mode,
            });
        }

        if positional.is_empty() {
            return Err("Missing archive name argument.".to_string());
        }

        let archive = PathBuf::from(&positional[0]);
        let files = positional[1..].iter().map(PathBuf::from).collect();

        Ok(Self {
            command,
            archive,
            files,
            output_dir,
            password,
            compression_level,
            solid,
            dict_size,
            threads,
            recurse,
            assume_yes,
            overwrite_mode,
        })
    }

    fn default_help() -> Self {
        Self {
            command: Command::Help,
            archive: PathBuf::new(),
            files: Vec::new(),
            output_dir: PathBuf::from("."),
            password: None,
            compression_level: 5,
            solid: true,
            dict_size: None,
            threads: None,
            recurse: false,
            assume_yes: false,
            overwrite_mode: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_dict_size() {
        assert_eq!(parse_dict_size("512k"), Some(512 * 1024));
        assert_eq!(parse_dict_size("16m"), Some(16 * 1024 * 1024));
        assert_eq!(parse_dict_size("64MB"), Some(64 * 1024 * 1024));
        assert_eq!(parse_dict_size("1g"), Some(1024 * 1024 * 1024));
        // 7-Zip bit exponents (<= 32)
        assert_eq!(parse_dict_size("24"), Some(1 << 24)); // 16 MB
        assert_eq!(parse_dict_size("26"), Some(1 << 26)); // 64 MB
        assert_eq!(parse_dict_size("31"), Some(1 << 31)); // 2 GB
        // 4 GB is one past u32::MAX; it is clamped by the LZMA2 encoder.
        assert_eq!(parse_dict_size("32"), Some(u32::MAX));
        assert_eq!(parse_dict_size("4g"), Some(u32::MAX));
        assert_eq!(parse_dict_size("4096m"), Some(u32::MAX));
        assert_eq!(parse_dict_size("4294967296"), Some(u32::MAX));
        assert_eq!(parse_dict_size("8g"), None);
        assert_eq!(parse_dict_size("4294967297"), None);
        // Raw bytes (> 32)
        assert_eq!(parse_dict_size("1048576"), Some(1048576));
    }

    #[test]
    fn test_parse_compression_switches() {
        let args = CliArgs::parse_from([
            "openarchive",
            "a",
            "test.7z",
            "file.txt",
            "-mx=9",
            "-md=32m",
            "-ms=off",
            "-mmt=4",
        ])
        .unwrap();

        assert_eq!(args.compression_level, 9);
        assert_eq!(args.dict_size, Some(32 * 1024 * 1024));
        assert_eq!(args.solid, false);
        assert_eq!(args.threads, Some(4));
    }

    #[test]
    fn test_parse_solid_variations() {
        let args_on =
            CliArgs::parse_from(["openarchive", "a", "test.7z", "file.txt", "-ms=on"]).unwrap();
        assert!(args_on.solid);

        let args_off =
            CliArgs::parse_from(["openarchive", "a", "test.7z", "file.txt", "-ms=off"]).unwrap();
        assert!(!args_off.solid);

        let args_e =
            CliArgs::parse_from(["openarchive", "a", "test.7z", "file.txt", "-ms=e"]).unwrap();
        assert!(!args_e.solid);

        let args_minus =
            CliArgs::parse_from(["openarchive", "a", "test.7z", "file.txt", "-ms-"]).unwrap();
        assert!(!args_minus.solid);
    }

    #[test]
    fn test_parse_threads_variations() {
        let args_1 =
            CliArgs::parse_from(["openarchive", "a", "test.7z", "file.txt", "-mmt=1"]).unwrap();
        assert_eq!(args_1.threads, Some(1));

        let args_off =
            CliArgs::parse_from(["openarchive", "a", "test.7z", "file.txt", "-mmt=off"]).unwrap();
        assert_eq!(args_off.threads, Some(1));

        let args_on =
            CliArgs::parse_from(["openarchive", "a", "test.7z", "file.txt", "-mmt=on"]).unwrap();
        assert!(args_on.threads.unwrap() >= 1);
    }
}
