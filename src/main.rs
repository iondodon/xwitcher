mod app;
mod atoms;
mod config;
mod icons;
mod overlay;
mod style;
mod util;

use crate::app::AltTab;
use crate::atoms::Atoms;
use crate::config::CliOptions;
use crate::style::Style;
use anyhow::{Context, Result};

fn main() -> Result<()> {
    let options = CliOptions::parse_env_args()?;
    let style = Style::load_from_config()?;
    let (conn, screen_num) = x11rb::connect(None).context("failed to connect to X server")?;
    let atoms = Atoms::new(&conn).context("failed to intern atoms")?;
    let mut app = AltTab::new(conn, screen_num, atoms, options.layout, style)?;
    app.run()
}
