use std::fs;
use std::path::PathBuf;

use clap::Args as ClapArgs;

#[derive(ClapArgs)]
pub struct Args {
    /// Lineage II encrypted package; any filename extension is accepted.
    #[arg(short, long)]
    input: PathBuf,

    /// Destination for the decrypted Unreal package.
    #[arg(short, long)]
    output: PathBuf,
}

pub fn run(args: Args) -> Result<(), String> {
    let bytes = fs::read(&args.input)
        .map_err(|error| format!("cannot read {}: {error}", args.input.display()))?;
    let filename = args
        .input
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    let decrypted = unreal_package::unreal::decrypt(bytes, filename)?;
    fs::write(&args.output, decrypted)
        .map_err(|error| format!("cannot write {}: {error}", args.output.display()))?;
    eprintln!(
        "decrypted {} to {}",
        args.input.display(),
        args.output.display()
    );
    Ok(())
}
