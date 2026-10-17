use std::path::PathBuf;

use clap::Args as ClapArgs;

#[derive(ClapArgs)]
pub struct Args {
    /// Unreal map package to render.
    #[arg(short, long)]
    pub input: PathBuf,
    /// Directory containing external .usx packages; may be specified more than once.
    #[arg(long, value_name = "PATH")]
    pub assets: Vec<PathBuf>,
}

#[cfg(feature = "viewer")]
pub fn run(args: Args) -> Result<(), String> {
    unreal_viewer::run(&args.input, &args.assets)
}

#[cfg(not(feature = "viewer"))]
pub fn run(_args: Args) -> Result<(), String> {
    Err("map viewing is disabled; rebuild with `cargo build --features viewer`".into())
}
