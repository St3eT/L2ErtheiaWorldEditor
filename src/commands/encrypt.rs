use std::fs;
use std::path::PathBuf;

use clap::Args as ClapArgs;

#[derive(ClapArgs)]
pub struct Args {
    /// Unencrypted Unreal package; any filename extension is accepted.
    #[arg(short, long)]
    input: PathBuf,

    /// Destination for the Lineage2Ver111-encrypted package.
    #[arg(short, long)]
    output: PathBuf,
}

pub fn run(args: Args) -> Result<(), String> {
    let bytes = fs::read(&args.input)
        .map_err(|error| format!("cannot read {}: {error}", args.input.display()))?;
    let encrypted = unreal_package::unreal::encrypt(bytes)?;
    fs::write(&args.output, encrypted)
        .map_err(|error| format!("cannot write {}: {error}", args.output.display()))?;
    eprintln!(
        "encrypted {} to {}",
        args.input.display(),
        args.output.display()
    );
    Ok(())
}
