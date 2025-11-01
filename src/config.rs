use anyhow::{Result, bail};
use std::env;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Layout {
    Horizontal,
    Vertical,
}

impl Default for Layout {
    fn default() -> Self {
        Layout::Horizontal
    }
}

pub struct CliOptions {
    pub layout: Layout,
}

impl CliOptions {
    pub fn parse<I>(args: I) -> Result<Self>
    where
        I: IntoIterator<Item = String>,
    {
        let mut layout = Layout::default();

        let mut iter = args.into_iter();
        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "-h" | "--horizontal" => layout = Layout::Horizontal,
                "-v" | "--vertical" => layout = Layout::Vertical,
                "-c" | "--css" => {
                    bail!("--css is no longer supported, use ~/.config/xwitcher/style.css");
                }
                _ => bail!("unknown option: {arg}"),
            }
        }

        Ok(Self { layout })
    }

    pub fn parse_env_args() -> Result<Self> {
        Self::parse(env::args().skip(1))
    }
}
