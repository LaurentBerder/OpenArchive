use std::path::PathBuf;

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
    pub recurse: bool,
    pub assume_yes: bool,
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
                recurse: false,
                assume_yes: false,
            });
        }

        let mut command_str = String::new();
        let mut positional = Vec::new();
        let mut output_dir = PathBuf::from(".");
        let mut password = None;
        let mut compression_level = 5;
        let mut recurse = false;
        let mut assume_yes = false;

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
                let val = arg[3..].trim_start_matches('=').to_string();
                if let Ok(lvl) = val.parse::<u8>() {
                    compression_level = lvl.clamp(0, 9);
                }
            } else if arg == "-r" || arg == "-r-" {
                recurse = arg == "-r";
            } else if arg == "-y" {
                assume_yes = true;
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
                if candidate.exists() || command_str.ends_with(".7z") || command_str.ends_with(".zip") {
                    Command::Gui(Some(candidate))
                } else {
                    return Err(format!("Unknown command: '{}'. Use 'a', 'x', 'e', 'l', 't', or 'gui'.", command_str));
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
                recurse,
                assume_yes,
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
            recurse,
            assume_yes,
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
            recurse: false,
            assume_yes: false,
        }
    }
}
