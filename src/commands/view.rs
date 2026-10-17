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
    /// Start the camera at these Unreal coordinates, e.g. `146745,25799,-1800`.
    #[arg(long, value_name = "X,Y,Z", value_parser = parse_point)]
    pub camera: Option<[f32; 3]>,
    /// Point the camera at these Unreal coordinates.
    #[arg(long, value_name = "X,Y,Z", value_parser = parse_point, requires = "camera")]
    pub look_at: Option<[f32; 3]>,
    /// Save a PNG of the first view once the map has loaded, then exit.
    #[arg(long, value_name = "PNG")]
    pub screenshot: Option<PathBuf>,
}

fn parse_point(value: &str) -> Result<[f32; 3], String> {
    let parts = value
        .split(',')
        .map(|part| part.trim().parse::<f32>().map_err(|error| error.to_string()))
        .collect::<Result<Vec<_>, _>>()?;
    parts
        .try_into()
        .map_err(|_| format!("expected X,Y,Z but got {value:?}"))
}

#[cfg(feature = "viewer")]
pub fn run(args: Args) -> Result<(), String> {
    let options = unreal_viewer::ViewOptions {
        camera: args.camera,
        look_at: args.look_at,
        screenshot: args.screenshot,
    };
    unreal_viewer::run(&args.input, &args.assets, options)
}

#[cfg(not(feature = "viewer"))]
pub fn run(_args: Args) -> Result<(), String> {
    Err("map viewing is disabled; rebuild with `cargo build --features viewer`".into())
}
