use std::fs;
use std::path::{Component, Path, PathBuf};

use clap::Args as ClapArgs;
use serde::Deserialize;

use unreal_package::unreal::{ExportDump, Package};

use crate::PackageKind;

#[derive(ClapArgs)]
pub struct Args {
    /// JSON directory previously produced by `unreal-rs unr export` or `unreal-rs ukx export`.
    #[arg(short, long)]
    input: PathBuf,

    /// Destination package.
    #[arg(short, long)]
    output: PathBuf,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Manifest {
    packages: Vec<ManifestPackage>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ManifestPackage {
    file: String,
    wire_resource: String,
    wire_size: u64,
    exports: Vec<ManifestExport>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ManifestExport {
    index: usize,
    resource: Option<String>,
    data: Option<ExportDump>,
}

pub fn run(args: Args, package_kind: PackageKind) -> Result<(), String> {
    let manifest_path = args.input.join("manifest.json");
    let manifest: Manifest = serde_json::from_slice(
        &fs::read(&manifest_path)
            .map_err(|error| format!("cannot read {}: {error}", manifest_path.display()))?,
    )
    .map_err(|error| format!("cannot parse {}: {error}", manifest_path.display()))?;
    if manifest.packages.len() != 1 {
        return Err(format!(
            "{} import requires exactly one package, found {}",
            package_kind.extension().to_ascii_uppercase(),
            manifest.packages.len()
        ));
    }
    let manifest = manifest.packages.into_iter().next().unwrap();
    let wire_path = resource_path(&args.input, &manifest.wire_resource)?;
    let wire = fs::read(&wire_path)
        .map_err(|error| format!("cannot read {}: {error}", wire_path.display()))?;
    if wire.len() as u64 != manifest.wire_size {
        return Err(format!(
            "wire source size is {}, expected {}",
            wire.len(),
            manifest.wire_size
        ));
    }

    let original_path = Path::new(&manifest.file);
    let package = Package::from_wire_bytes(original_path, wire.clone())?;
    if manifest.exports.len() != package.export_count() {
        return Err(format!(
            "manifest has {} exports but wire source has {}",
            manifest.exports.len(),
            package.export_count()
        ));
    }
    let mut exports = Vec::with_capacity(manifest.exports.len());
    for (expected_index, export) in manifest.exports.into_iter().enumerate() {
        if export.index != expected_index {
            return Err(format!(
                "manifest export index {} appears at position {expected_index}",
                export.index
            ));
        }
        let data = match (export.data, export.resource) {
            (Some(data), None) => data,
            (None, Some(resource)) => {
                let path = resource_path(&args.input, &resource)?;
                serde_json::from_slice(
                    &fs::read(&path)
                        .map_err(|error| format!("cannot read {}: {error}", path.display()))?,
                )
                .map_err(|error| format!("cannot parse {}: {error}", path.display()))?
            }
            (Some(_), Some(_)) => {
                return Err(format!("export {expected_index} has data and resource"));
            }
            (None, None) => return Err(format!("export {expected_index} has no data or resource")),
        };
        exports.push(Some(data));
    }

    let plain = package.rebuild_from_dumps(&exports)?;
    let filename = original_path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    let output = unreal_package::unreal::reencrypt_like(&wire, plain, filename)?;
    if let Some(parent) = args
        .output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    }
    fs::write(&args.output, output)
        .map_err(|error| format!("cannot write {}: {error}", args.output.display()))?;
    eprintln!(
        "rebuilt {} from {}",
        args.output.display(),
        args.input.display()
    );
    Ok(())
}

fn resource_path(root: &Path, relative: &str) -> Result<PathBuf, String> {
    let relative = Path::new(relative);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(format!(
            "resource path {relative:?} is not a safe relative path"
        ));
    }
    Ok(root.join(relative))
}
