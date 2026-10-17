use std::path::PathBuf;

use clap::Args as ClapArgs;

use crate::PackageKind;

#[derive(ClapArgs)]
pub struct Args {
    /// Unreal package file or directory to export.
    #[arg(short, long)]
    pub input: PathBuf,

    /// XML/JSON file or output directory.
    #[arg(short, long)]
    pub output: PathBuf,

    /// Output format; inferred from a file output extension when omitted.
    #[arg(short, long, value_parser = ["xml", "json"])]
    pub format: Option<String>,

    /// Externalize exports at least this many bytes (zero externalizes all).
    #[arg(long, default_value_t = crate::DEFAULT_SPLIT_THRESHOLD)]
    pub split_threshold: usize,
}

pub fn run(args: Args, package_kind: PackageKind) -> Result<(), String> {
    crate::export_packages(args, package_kind)
}
