#![windows_subsystem = "windows"]

use std::env;
use std::path::PathBuf;

fn main() -> anyhow::Result<()> {
    let arg = env::args().nth(1).map(PathBuf::from);
    openarchive::gui::run_gui(arg).map_err(|e| anyhow::anyhow!("{}", e))?;
    Ok(())
}
