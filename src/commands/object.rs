use std::fs;
use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};
use serde::Deserialize;

use unreal_package::unreal::{ExportDump, ObjectBundle, Package};

#[derive(Subcommand)]
pub enum Command {
    /// Extract an export and its package-table dependencies into a JSON bundle.
    Export(ExportArgs),
    /// Append an export from a portable bundle or JSON directory-export resource.
    AddExport(AddExportArgs),
    /// Append an import table entry, creating its package outers as needed.
    AddImport(AddImportArgs),
    /// Remove an unreferenced export using an index-stable tombstone.
    RemoveExport(RemoveArgs),
    /// Remove an unreferenced import using an index-stable tombstone.
    RemoveImport(RemoveArgs),
}

#[derive(Args)]
pub struct ExportArgs {
    #[arg(short, long)]
    input: PathBuf,
    /// Full object path, object name, or one-based export reference (`#123`).
    #[arg(long)]
    object: String,
    #[arg(short, long)]
    output: PathBuf,
}

#[derive(Args)]
pub struct AddExportArgs {
    #[arg(short, long)]
    input: PathBuf,
    /// Portable object bundle, or a resource kept beside its manifest.json and wire directory.
    #[arg(long)]
    bundle: PathBuf,
    /// Override the exported object's name. The name must exist in the bundle.
    #[arg(long)]
    name: Option<String>,
    #[arg(short, long)]
    output: PathBuf,
}

#[derive(Args)]
pub struct AddImportArgs {
    #[arg(short, long)]
    input: PathBuf,
    /// Imported object path, for example `MonSound6.Grim_Reaper.attack`.
    #[arg(long)]
    path: String,
    /// Import class, for example `Engine.Sound`.
    #[arg(long, default_value = "Core.Package")]
    class: String,
    #[arg(short, long)]
    output: PathBuf,
}

#[derive(Args)]
pub struct RemoveArgs {
    #[arg(short, long)]
    input: PathBuf,
    /// Full object path, object name, or signed reference (`#123` or `#-123`).
    #[arg(long)]
    object: String,
    #[arg(short, long)]
    output: PathBuf,
}

pub fn run(command: Command) -> Result<(), String> {
    match command {
        Command::Export(args) => export(args),
        Command::AddExport(args) => mutate(&args.input, &args.output, |package| {
            let bundle = load_bundle(&args.bundle)?;
            package.add_export(&bundle, args.name.as_deref())
        }),
        Command::AddImport(args) => mutate(&args.input, &args.output, |package| {
            package.add_import(&args.path, &args.class)
        }),
        Command::RemoveExport(args) => mutate(&args.input, &args.output, |package| {
            package.remove_export(&args.object)
        }),
        Command::RemoveImport(args) => mutate(&args.input, &args.output, |package| {
            package.remove_import(&args.object)
        }),
    }
}

fn export(args: ExportArgs) -> Result<(), String> {
    let package = Package::read(&args.input)?;
    let bundle = package.object_bundle(&args.object)?;
    ensure_parent(&args.output)?;
    let mut bytes = serde_json::to_vec_pretty(&bundle).map_err(|error| error.to_string())?;
    bytes.push(b'\n');
    fs::write(&args.output, bytes)
        .map_err(|error| format!("cannot write {}: {error}", args.output.display()))?;
    eprintln!("exported {} to {}", args.object, args.output.display());
    Ok(())
}

fn mutate(
    input: &Path,
    output: &Path,
    operation: impl FnOnce(&Package) -> Result<Vec<u8>, String>,
) -> Result<(), String> {
    let wire =
        fs::read(input).map_err(|error| format!("cannot read {}: {error}", input.display()))?;
    let package = Package::from_wire_bytes(input, wire.clone())?;
    let plain = operation(&package)?;
    let filename = input
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    let output_bytes = unreal_package::unreal::reencrypt_like(&wire, plain, filename)?;
    ensure_parent(output)?;
    fs::write(output, output_bytes)
        .map_err(|error| format!("cannot write {}: {error}", output.display()))?;
    eprintln!("wrote {}", output.display());
    Ok(())
}

fn ensure_parent(path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent().filter(|path| !path.as_os_str().is_empty()) {
        fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    }
    Ok(())
}

enum BundleDocument {
    Portable(ObjectBundle),
    DirectoryExport(ExportDump),
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BundleProbe {
    format_version: Option<u32>,
    index: Option<usize>,
}

#[derive(Deserialize)]
struct DirectoryDocument {
    packages: Vec<DirectoryPackage>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DirectoryPackage {
    file: String,
    wire_resource: String,
    exports: Vec<DirectoryExport>,
}

#[derive(Deserialize)]
struct DirectoryExport {
    index: usize,
    resource: Option<String>,
}

fn parse_bundle_document(bytes: &[u8]) -> Result<BundleDocument, String> {
    let probe: BundleProbe =
        serde_json::from_slice(bytes).map_err(|error| format!("invalid JSON: {error}"))?;
    if probe.format_version.is_some() {
        return serde_json::from_slice(bytes)
            .map(BundleDocument::Portable)
            .map_err(|error| format!("invalid portable object bundle: {error}"));
    }
    if probe.index.is_some() {
        return serde_json::from_slice(bytes)
            .map(BundleDocument::DirectoryExport)
            .map_err(|error| format!("invalid directory-export resource: {error}"));
    }
    Err("JSON is neither a portable object bundle nor a directory-export resource".into())
}

fn load_bundle(path: &Path) -> Result<ObjectBundle, String> {
    let bytes =
        fs::read(path).map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    match parse_bundle_document(&bytes)
        .map_err(|error| format!("cannot parse {}: {error}", path.display()))?
    {
        BundleDocument::Portable(bundle) => Ok(bundle),
        BundleDocument::DirectoryExport(export) => directory_export_bundle(path, export),
    }
}

fn directory_export_bundle(path: &Path, export: ExportDump) -> Result<ObjectBundle, String> {
    let resource_path = fs::canonicalize(path)
        .map_err(|error| format!("cannot resolve {}: {error}", path.display()))?;
    let resource_parent = resource_path
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", path.display()))?;
    for root in path.parent().into_iter().flat_map(Path::ancestors) {
        let manifest_path = root.join("manifest.json");
        if !manifest_path.is_file() {
            continue;
        }
        let manifest: DirectoryDocument = serde_json::from_slice(
            &fs::read(&manifest_path)
                .map_err(|error| format!("cannot read {}: {error}", manifest_path.display()))?,
        )
        .map_err(|error| format!("cannot parse {}: {error}", manifest_path.display()))?;
        for package in manifest.packages {
            let exact_export = package.exports.iter().find(|listed| {
                listed.resource.as_ref().is_some_and(|resource| {
                    fs::canonicalize(root.join(resource)).is_ok_and(|value| value == resource_path)
                })
            });
            let listed_export = exact_export.or_else(|| {
                package.exports.iter().find(|listed| {
                    listed.index == export.index
                        && listed.resource.as_ref().is_some_and(|resource| {
                            root.join(resource)
                                .parent()
                                .and_then(|parent| fs::canonicalize(parent).ok())
                                .is_some_and(|parent| parent == resource_parent)
                        })
                })
            });
            let Some(listed_export) = listed_export else {
                continue;
            };
            if listed_export.index != export.index {
                return Err(format!(
                    "{} identifies export {}, but {} identifies export {}",
                    manifest_path.display(),
                    listed_export.index,
                    path.display(),
                    export.index
                ));
            }
            let wire_path = root.join(&package.wire_resource);
            let wire = fs::read(&wire_path)
                .map_err(|error| format!("cannot read {}: {error}", wire_path.display()))?;
            let source =
                Package::from_wire_bytes(Path::new(&package.file), wire).map_err(|error| {
                    format!(
                        "cannot load preserved source {}: {error}",
                        wire_path.display()
                    )
                })?;
            let mut bundle = source.object_bundle(&format!("#{}", export.reference))?;
            bundle.export = export;
            return Ok(bundle);
        }
    }
    Err(format!(
        "{} is a directory-export resource, but no adjacent manifest.json references it; keep the resource with its manifest and wire directory, or create a portable bundle with `unreal-rs ukx object export`",
        path.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_directory_export_resource_json() {
        let json = br#"{
            "index": 3,
            "reference": 4,
            "name": "Example",
            "fullName": "Example",
            "class": "Engine.SkeletalMesh",
            "superClass": "",
            "outer": 0,
            "flags": "0x00000000",
            "size": 0,
            "offset": 0,
            "properties": [],
            "nativeObject": null,
            "unreadData": "",
            "rawData": ""
        }"#;

        match parse_bundle_document(json).unwrap() {
            BundleDocument::DirectoryExport(export) => assert_eq!(export.name, "Example"),
            BundleDocument::Portable(_) => panic!("resource JSON was mistaken for a bundle"),
        }
    }
}
