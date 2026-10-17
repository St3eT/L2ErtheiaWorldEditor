mod commands;

use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use serde::Serialize;
use unreal::{DumpDocument, ExportDump, PackageDump};
use unreal_package::unreal;

pub(crate) const DEFAULT_SPLIT_THRESHOLD: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PackageKind {
    Unr,
    Ukx,
}

#[derive(Parser)]
#[command(version, about = "Extract and rebuild Unreal Engine 2 packages")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Work with Unreal map packages.
    Unr {
        #[command(subcommand)]
        command: PackageCommand,
    },
    /// Work with Unreal animation packages.
    Ukx {
        #[command(subcommand)]
        command: PackageCommand,
    },
    /// Encrypt an Unreal package using Lineage2Ver111.
    Encrypt(commands::encrypt::Args),
    /// Decrypt a Lineage II Unreal package.
    Decrypt(commands::decrypt::Args),
}

#[derive(Subcommand)]
enum PackageCommand {
    /// Export package contents to XML or JSON.
    Export(commands::export::Args),
    /// Rebuild a package from a JSON directory export.
    Import(commands::import::Args),
    /// Extract, add, or remove individual package objects.
    Object {
        #[command(subcommand)]
        command: commands::object::Command,
    },
    /// Render a map in an interactive 3D viewer.
    View(commands::view::Args),
}

impl PackageKind {
    fn extension(self) -> &'static str {
        match self {
            Self::Unr => "unr",
            Self::Ukx => "ukx",
        }
    }

    fn installation_directory(self) -> &'static str {
        match self {
            Self::Unr => "MAPS",
            Self::Ukx => "ANIMATIONS",
        }
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    match Cli::parse().command {
        Command::Unr {
            command: PackageCommand::Export(args),
        } => commands::export::run(args, PackageKind::Unr),
        Command::Ukx {
            command: PackageCommand::Export(args),
        } => commands::export::run(args, PackageKind::Ukx),
        Command::Unr {
            command: PackageCommand::Import(args),
        } => commands::import::run(args, PackageKind::Unr),
        Command::Ukx {
            command: PackageCommand::Import(args),
        } => commands::import::run(args, PackageKind::Ukx),
        Command::Ukx {
            command: PackageCommand::Object { command },
        } => commands::object::run(command),
        Command::Unr {
            command: PackageCommand::View(args),
        } => commands::view::run(args),
        Command::Ukx {
            command: PackageCommand::View(_),
        } => Err("UKX packages cannot be opened as maps".into()),
        Command::Unr {
            command: PackageCommand::Object { .. },
        } => Err("UNR object editing is not supported yet".into()),
        Command::Encrypt(args) => commands::encrypt::run(args),
        Command::Decrypt(args) => commands::decrypt::run(args),
    }
}

pub(crate) fn export_packages(
    args: commands::export::Args,
    package_kind: PackageKind,
) -> Result<(), String> {
    let commands::export::Args {
        input,
        output,
        format,
        split_threshold,
    } = args;
    let directory_output = output.is_dir() || output.extension().is_none();
    let format = format.unwrap_or_else(|| {
        output
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("")
            .to_ascii_lowercase()
    });
    if format != "xml" && format != "json" {
        return Err(
            "output extension must be .xml or .json; directory output requires --format".into(),
        );
    }
    let files = discover(&input, package_kind)?;
    if files.is_empty() {
        return Err(format!(
            "no .{} files found under {}",
            package_kind.extension(),
            input.display()
        ));
    }

    let mut packages = Vec::new();
    for file in &files {
        match unreal::Package::read(file) {
            Ok(package) => {
                eprintln!("{}: {} export(s)", file.display(), package.export_count());
                packages.push(package.dump());
            }
            Err(error) => eprintln!("{}: {error}", file.display()),
        }
    }
    if packages.is_empty() {
        return Err("all input files failed to parse".into());
    }
    let succeeded = packages.len();
    if directory_output {
        write_directory(&output, &format, &packages, split_threshold)?;
        eprintln!("wrote {succeeded} package(s) under {}", output.display());
        return Ok(());
    }
    let document = DumpDocument { packages };
    let mut out = BufWriter::new(
        File::create(&output).map_err(|e| format!("cannot create {}: {e}", output.display()))?,
    );
    if format == "json" {
        serde_json::to_writer_pretty(&mut out, &document).map_err(|e| e.to_string())?;
        writeln!(out).map_err(io_error)?;
    } else {
        let mut body = String::new();
        let mut serializer =
            quick_xml::se::Serializer::with_root(&mut body, Some("unrealPackages"))
                .map_err(|e| e.to_string())?;
        serializer.indent(' ', 2);
        document.serialize(serializer).map_err(|e| e.to_string())?;
        writeln!(out, "<?xml version=\"1.0\" encoding=\"UTF-8\"?>").map_err(io_error)?;
        out.write_all(body.as_bytes()).map_err(io_error)?;
        writeln!(out).map_err(io_error)?;
    }
    out.flush().map_err(io_error)?;
    eprintln!("wrote {succeeded} package(s) to {}", output.display());
    Ok(())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DirectoryDocument<'a> {
    packages: Vec<DirectoryPackage<'a>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DirectoryPackage<'a> {
    file: &'a str,
    wire_resource: String,
    wire_size: u64,
    version: u16,
    license: u16,
    flags: &'a str,
    names: &'a [unreal::NameDump],
    imports: &'a [unreal::ImportDump],
    exports: Vec<DirectoryExport<'a>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DirectoryExport<'a> {
    index: usize,
    name: &'a str,
    class: &'a str,
    size: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    resource: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<&'a ExportDump>,
}

fn write_directory(
    output: &Path,
    format: &str,
    packages: &[PackageDump],
    threshold: usize,
) -> Result<(), String> {
    fs::create_dir_all(output).map_err(|e| format!("cannot create {}: {e}", output.display()))?;
    let extension = format;
    let mut manifest_packages = Vec::with_capacity(packages.len());
    for (package_index, package) in packages.iter().enumerate() {
        let package_name = Path::new(&package.file)
            .file_stem()
            .and_then(|value| value.to_str())
            .map(safe_name)
            .unwrap_or_else(|| format!("package-{package_index:04}"));
        let (wire_resource, wire_size) = write_wire_source(
            output,
            Path::new(&package.file),
            package_index,
            &package_name,
        )?;
        let resource_dir = output.join("resources").join(&package_name);
        let mut exports = Vec::with_capacity(package.exports.len());
        for export in &package.exports {
            let external = threshold == 0 || serialized_size(export, format)? >= threshold;
            let resource = if external {
                fs::create_dir_all(&resource_dir)
                    .map_err(|e| format!("cannot create {}: {e}", resource_dir.display()))?;
                let filename = format!(
                    "{:06}-{}.{}",
                    export.index,
                    safe_name(&export.name),
                    extension
                );
                let relative = PathBuf::from("resources")
                    .join(&package_name)
                    .join(filename);
                write_serialized(&output.join(&relative), format, "unrealExport", export)?;
                Some(relative.to_string_lossy().replace('\\', "/"))
            } else {
                None
            };
            exports.push(DirectoryExport {
                index: export.index,
                name: &export.name,
                class: &export.class,
                size: export.size,
                data: resource.is_none().then_some(export),
                resource,
            });
        }
        manifest_packages.push(DirectoryPackage {
            file: &package.file,
            wire_resource,
            wire_size,
            version: package.version,
            license: package.license,
            flags: &package.flags,
            names: &package.names,
            imports: &package.imports,
            exports,
        });
    }
    let manifest = DirectoryDocument {
        packages: manifest_packages,
    };
    write_serialized(
        &output.join(format!("manifest.{extension}")),
        format,
        "unrealPackages",
        &manifest,
    )
}

fn write_wire_source(
    output: &Path,
    source: &Path,
    package_index: usize,
    package_name: &str,
) -> Result<(String, u64), String> {
    let relative = PathBuf::from("wire").join(format!("{package_index:04}-{package_name}.bin"));
    let destination = output.join(&relative);
    let parent = destination
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", destination.display()))?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    let wire_size = fs::copy(source, &destination).map_err(|error| {
        format!(
            "cannot preserve wire source {} as {}: {error}",
            source.display(),
            destination.display()
        )
    })?;
    Ok((relative.to_string_lossy().replace('\\', "/"), wire_size))
}

fn serialized_size<T: Serialize>(value: &T, format: &str) -> Result<usize, String> {
    if format == "json" {
        serde_json::to_vec_pretty(value)
            .map(|bytes| bytes.len())
            .map_err(|e| e.to_string())
    } else {
        let mut body = String::new();
        let mut serializer = quick_xml::se::Serializer::with_root(&mut body, Some("unrealExport"))
            .map_err(|e| e.to_string())?;
        serializer.indent(' ', 2);
        value.serialize(serializer).map_err(|e| e.to_string())?;
        Ok(body.len())
    }
}

fn safe_name(value: &str) -> String {
    let result: String = value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if result.is_empty() {
        "unnamed".into()
    } else {
        result
    }
}

fn write_serialized<T: Serialize>(
    path: &Path,
    format: &str,
    xml_root: &str,
    value: &T,
) -> Result<(), String> {
    let mut out = BufWriter::new(
        File::create(path).map_err(|e| format!("cannot create {}: {e}", path.display()))?,
    );
    if format == "json" {
        serde_json::to_writer_pretty(&mut out, value).map_err(|e| e.to_string())?;
        writeln!(out).map_err(io_error)?;
    } else {
        let mut body = String::new();
        let mut serializer = quick_xml::se::Serializer::with_root(&mut body, Some(xml_root))
            .map_err(|e| e.to_string())?;
        serializer.indent(' ', 2);
        value.serialize(serializer).map_err(|e| e.to_string())?;
        writeln!(out, "<?xml version=\"1.0\" encoding=\"UTF-8\"?>").map_err(io_error)?;
        out.write_all(body.as_bytes()).map_err(io_error)?;
        writeln!(out).map_err(io_error)?;
    }
    out.flush().map_err(io_error)
}

fn discover(input: &Path, package_kind: PackageKind) -> Result<Vec<PathBuf>, String> {
    if input.is_file() {
        return Ok(vec![input.to_path_buf()]);
    }
    if !input.is_dir() {
        return Err(format!("{} does not exist", input.display()));
    }
    let package_directory = input.join(package_kind.installation_directory());
    let directory = if package_directory.is_dir() {
        package_directory
    } else {
        input.into()
    };
    let mut files = Vec::new();
    for entry in
        fs::read_dir(&directory).map_err(|e| format!("cannot read {}: {e}", directory.display()))?
    {
        let path = entry.map_err(|e| e.to_string())?.path();
        if path.is_file()
            && path
                .extension()
                .is_some_and(|x| x.eq_ignore_ascii_case(package_kind.extension()))
        {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

fn io_error(error: std::io::Error) -> String {
    error.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn directory_export_preserves_exact_wire_source() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "unreal-rs-wire-source-{}-{unique}",
            std::process::id()
        ));
        let source = root.join("source.ukx");
        let output = root.join("decoded");
        let original = [0x4c, 0x00, 0x69, 0x00, 0xff, 0x81, 0x00];
        fs::create_dir_all(&root).unwrap();
        fs::write(&source, original).unwrap();

        let package = PackageDump {
            file: source.display().to_string(),
            version: 123,
            license: 37,
            flags: "0x00000001".into(),
            names: Vec::new(),
            imports: Vec::new(),
            exports: Vec::new(),
        };
        write_directory(&output, "json", &[package], DEFAULT_SPLIT_THRESHOLD).unwrap();

        let wire_path = output.join("wire/0000-source.bin");
        assert_eq!(fs::read(&wire_path).unwrap(), original);
        let manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(output.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(
            manifest["packages"][0]["wireResource"],
            "wire/0000-source.bin"
        );
        assert_eq!(manifest["packages"][0]["wireSize"], original.len());

        fs::remove_dir_all(root).unwrap();
    }
}
