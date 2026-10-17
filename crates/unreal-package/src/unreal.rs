use std::fs;
use std::path::{Path, PathBuf};

use crate::codec::{BinaryReader, BinaryWriter, CompactInt, encode_property_size};
use crate::model::{CompactNameRef, CompactObjectRef, ObjectRef, PackageVersion};
use crate::native::{NativeContext, NativeObject, NativeObjectRegistry};
use serde::{Deserialize, Serialize};

const MAGIC: u32 = 0x9e2a83c1;
const HAS_STACK: u32 = 1 << 25;

pub struct Package {
    path: PathBuf,
    data: Vec<u8>,
    version: PackageVersion,
    flags: u32,
    names: Vec<Name>,
    imports: Vec<Import>,
    exports: Vec<Export>,
}

#[derive(Clone)]
struct Name {
    value: String,
    flags: u32,
}
#[derive(Clone)]
struct Import {
    class_package: CompactNameRef,
    class_name: CompactNameRef,
    outer: ObjectRef,
    name: CompactNameRef,
}
#[derive(Clone)]
struct Export {
    class: CompactObjectRef,
    super_class: CompactObjectRef,
    outer: ObjectRef,
    name: CompactNameRef,
    flags: u32,
    size: usize,
    offset: usize,
}

impl Package {
    pub fn read(path: &Path) -> Result<Self, String> {
        let bytes = fs::read(path).map_err(|e| e.to_string())?;
        Self::from_wire_bytes(path, bytes)
    }

    pub fn from_wire_bytes(path: &Path, bytes: Vec<u8>) -> Result<Self, String> {
        let data = decrypt(
            bytes,
            path.file_name().and_then(|x| x.to_str()).unwrap_or(""),
        )?;
        let mut h = Cursor::new(&data, 0, data.len());
        if h.u32()? != MAGIC {
            return Err("not an Unreal package".into());
        }
        let version = PackageVersion::new(h.u16()?, h.u16()?);
        let flags = h.u32()?;
        let name_count = h.count("name")?;
        let name_offset = h.offset("name")?;
        let export_count = h.count("export")?;
        let export_offset = h.offset("export")?;
        let import_count = h.count("import")?;
        let import_offset = h.offset("import")?;

        let mut c = Cursor::new(&data, name_offset, data.len());
        let mut names = Vec::with_capacity(name_count);
        for _ in 0..name_count {
            names.push(Name {
                value: c.unreal_string()?,
                flags: c.u32()?,
            });
        }

        c = Cursor::new(&data, import_offset, data.len());
        let mut imports = Vec::with_capacity(import_count);
        for _ in 0..import_count {
            imports.push(Import {
                class_package: CompactNameRef::checked(
                    c.compact_value()?,
                    names.len(),
                    "import class package",
                )?,
                class_name: CompactNameRef::checked(
                    c.compact_value()?,
                    names.len(),
                    "import class name",
                )?,
                outer: ObjectRef::checked(c.i32()?, import_count, export_count, "import outer")?,
                name: CompactNameRef::checked(c.compact_value()?, names.len(), "import name")?,
            });
        }
        c = Cursor::new(&data, export_offset, data.len());
        let mut exports = Vec::with_capacity(export_count);
        for index in 0..export_count {
            let class = CompactObjectRef::checked(
                c.compact_value()
                    .map_err(|error| export_error(index, "class", error))?,
                import_count,
                export_count,
                "export class",
            )?;
            let super_class = CompactObjectRef::checked(
                c.compact_value()
                    .map_err(|error| export_error(index, "superclass", error))?,
                import_count,
                export_count,
                "export superclass",
            )?;
            let outer = ObjectRef::checked(
                c.i32()
                    .map_err(|error| export_error(index, "outer", error))?,
                import_count,
                export_count,
                "export outer",
            )?;
            let name = CompactNameRef::checked(
                c.compact_value()
                    .map_err(|error| export_error(index, "name", error))?,
                names.len(),
                "export name",
            )?;
            let object_flags = c
                .u32()
                .map_err(|error| export_error(index, "flags", error))?;
            let size = c
                .nonnegative_compact("export size")
                .map_err(|error| export_error(index, "size", error))?;
            let offset = c
                .nonnegative_compact("export offset")
                .map_err(|error| export_error(index, "offset", error))?;
            if size > 0 && offset.checked_add(size).is_none_or(|end| end > data.len()) {
                return Err("export points outside package".into());
            }
            exports.push(Export {
                class,
                super_class,
                outer,
                name,
                flags: object_flags,
                size,
                offset,
            });
        }
        Ok(Self {
            path: path.into(),
            data,
            version,
            flags,
            names,
            imports,
            exports,
        })
    }

    pub fn rebuild_from_dumps(&self, edited: &[Option<ExportDump>]) -> Result<Vec<u8>, String> {
        if edited.len() != self.exports.len() {
            return Err(format!(
                "manifest has {} exports but wire source has {}",
                edited.len(),
                self.exports.len()
            ));
        }
        let original = self.dump();
        let mut data = self.data.clone();
        let mut locations: Vec<(usize, usize)> = self
            .exports
            .iter()
            .map(|export| (export.offset, export.size))
            .collect();
        let mut changed = false;

        for (index, edited) in edited.iter().enumerate() {
            let Some(edited) = edited else { continue };
            if semantic_export(edited)? == semantic_export(&original.exports[index])? {
                continue;
            }
            changed = true;
            let offset = data.len();
            let bytes = self.encode_export(edited, Some(&original.exports[index]), offset)?;
            locations[index] = (offset, bytes.len());
            data.extend_from_slice(&bytes);
        }

        if !changed {
            return Ok(data);
        }

        let export_offset = data.len();
        for (export, &(offset, size)) in self.exports.iter().zip(&locations) {
            let mut writer = BinaryWriter::default();
            export.class.wire.write(&mut writer);
            export.super_class.wire.write(&mut writer);
            writer.i32(export.outer.wire());
            export.name.wire.write(&mut writer);
            writer.u32(export.flags);
            writer.compact(i32::try_from(size).map_err(|_| "export size exceeds i32")?);
            writer.compact(i32::try_from(offset).map_err(|_| "export offset exceeds i32")?);
            data.extend_from_slice(&writer.into_bytes());
        }
        let export_offset = u32::try_from(export_offset).map_err(|_| "package exceeds u32")?;
        data[24..28].copy_from_slice(&export_offset.to_le_bytes());
        Ok(data)
    }

    fn encode_export(
        &self,
        edited: &ExportDump,
        original: Option<&ExportDump>,
        export_offset: usize,
    ) -> Result<Vec<u8>, String> {
        if let Some(raw) = &edited.raw_data {
            return decode_hex(raw);
        }
        let mut writer = BinaryWriter::default();
        if let Some(stack) = &edited.stack {
            writer.compact(stack.node);
            writer.compact(stack.state_node);
            writer.u32(stack.probe_mask as u32);
            writer.u32((stack.probe_mask >> 32) as u32);
            writer.i32(stack.latent_action);
            writer.compact(stack.state_offset);
        }
        for (index, property) in edited.properties.iter().enumerate() {
            let original_property = original.and_then(|original| original.properties.get(index));
            self.encode_property(&mut writer, property, original_property)?;
        }
        writer.compact(self.name_index("None")?);

        if let Some(native) = &edited.native_object {
            let context = crate::native_encode::Context {
                version: self.version,
                object_offset: export_offset + writer.position(),
                name_index: &|value| self.name_index(value),
                object_wire: &|value| self.object_wire(value),
            };
            writer.bytes(&crate::native_encode::encode(native, &context)?);
        }
        writer.bytes(&decode_hex(&edited.unread_data)?);
        Ok(writer.into_bytes())
    }

    fn encode_property(
        &self,
        writer: &mut BinaryWriter,
        property: &PropertyDump,
        original: Option<&PropertyDump>,
    ) -> Result<(), String> {
        let (kind, structure) = property_kind(&property.property_type)?;
        let raw = if original
            .is_some_and(|value| value.value != property.value && value.raw == property.raw)
        {
            self.encode_property_value(property, kind)?
        } else {
            decode_hex(&property.raw)?
        };
        if let Some(original) = original.filter(|original| {
            !original.wire_header.is_empty()
                && original.name == property.name
                && original.property_type == property.property_type
                && original.array_index == property.array_index
                && original.size == raw.len()
        }) {
            writer.bytes(&decode_hex(&original.wire_header)?);
            writer.bytes(&raw);
            return Ok(());
        }

        writer.compact(self.name_index(&property.name)?);
        let (size_code, extra) = encode_property_size(raw.len());
        let has_array_index = property.array_index != 0 && kind != 3;
        let mut info = kind | size_code << 4;
        if has_array_index || (kind == 3 && property.value == "true") {
            info |= 0x80;
        }
        writer.u8(info);
        if let Some(structure) = structure {
            writer.compact(self.name_index(structure)?);
        }
        writer.bytes(&extra);
        if has_array_index {
            writer.compact(property.array_index);
        }
        writer.bytes(&raw);
        Ok(())
    }

    fn encode_property_value(&self, property: &PropertyDump, kind: u8) -> Result<Vec<u8>, String> {
        match kind {
            1 => Ok(vec![
                property.value.parse::<u8>().map_err(|e| e.to_string())?,
            ]),
            2 => Ok(property
                .value
                .parse::<i32>()
                .map_err(|e| e.to_string())?
                .to_le_bytes()
                .to_vec()),
            4 => Ok(property
                .value
                .parse::<f32>()
                .map_err(|e| e.to_string())?
                .to_le_bytes()
                .to_vec()),
            5 => self.compact_bytes(self.object_wire(&property.value)?),
            6 => self.compact_bytes(self.name_index(&property.value)?),
            10..=12 => encode_triplet(&property.value, kind != 12),
            13 => {
                let mut value = property.value.as_bytes().to_vec();
                value.push(0);
                Ok(value)
            }
            _ => Err(format!(
                "editing {} property values is not supported; edit raw instead",
                property.property_type
            )),
        }
    }

    fn compact_bytes(&self, value: i32) -> Result<Vec<u8>, String> {
        let mut writer = BinaryWriter::default();
        writer.compact(value);
        Ok(writer.into_bytes())
    }

    fn name_index(&self, value: &str) -> Result<i32, String> {
        self.names
            .iter()
            .position(|name| name.value.eq_ignore_ascii_case(value))
            .and_then(|index| i32::try_from(index).ok())
            .ok_or_else(|| format!("name {value:?} is not present in the package name table"))
    }

    fn object_wire(&self, value: &str) -> Result<i32, String> {
        if value.is_empty() {
            return Ok(0);
        }
        for index in 0..self.exports.len() {
            let reference = ObjectRef::Export(crate::model::ExportIndex::from_usize(index));
            if self.object_path(reference).eq_ignore_ascii_case(value) {
                return Ok(reference.wire());
            }
        }
        for index in 0..self.imports.len() {
            let reference = ObjectRef::Import(crate::model::ImportIndex::from_usize(index));
            if self.object_path(reference).eq_ignore_ascii_case(value) {
                return Ok(reference.wire());
            }
        }
        Err(format!(
            "object {value:?} is not present in the package tables"
        ))
    }

    pub fn object_bundle(&self, selector: &str) -> Result<ObjectBundle, String> {
        let index = self.find_export(selector)?;
        let dump = self.dump();
        let mut selected = Vec::new();
        collect_export_dependencies(index, &dump.exports, &mut selected)?;
        selected.retain(|dependency| *dependency != index);
        let mut dependencies = Vec::with_capacity(selected.len());
        for &dependency in &selected {
            dependencies.push(clone_json(&dump.exports[dependency])?);
        }
        let mut values = Vec::with_capacity(selected.len() + 1);
        for &selected_index in selected.iter().chain(std::iter::once(&index)) {
            values.push(
                serde_json::to_value(&dump.exports[selected_index])
                    .map_err(|error| error.to_string())?,
            );
        }
        let mut required_imports = Vec::new();
        for (import_index, import) in dump.imports.iter().enumerate() {
            if values
                .iter()
                .any(|value| json_contains_string(value, &import.full_name))
            {
                collect_import_outers(import_index, &dump.imports, &mut required_imports);
            }
        }
        Ok(ObjectBundle {
            format_version: 1,
            archive_version: self.version.archive,
            license_version: self.version.license,
            names: dump.names,
            imports: dump
                .imports
                .into_iter()
                .enumerate()
                .filter(|(index, _)| required_imports.contains(index))
                .map(|(_, import)| import)
                .collect(),
            dependencies,
            export: clone_json(&dump.exports[index])?,
        })
    }

    pub fn add_import(&self, path: &str, class: &str) -> Result<Vec<u8>, String> {
        if self.object_wire(path).is_ok() {
            return Err(format!("object {path:?} already exists"));
        }
        let mut names = self.names.clone();
        let mut imports = self.imports.clone();
        let mut paths: Vec<String> = self
            .dump()
            .imports
            .into_iter()
            .map(|item| item.full_name)
            .collect();
        ensure_import_path(&mut names, &mut imports, &mut paths, path, class)?;
        self.rebuild_tables(self.data.clone(), names, imports, self.exports.clone())
    }

    pub fn add_export(
        &self,
        bundle: &ObjectBundle,
        name_override: Option<&str>,
    ) -> Result<Vec<u8>, String> {
        if bundle.format_version != 1 {
            return Err(format!(
                "unsupported object bundle version {}",
                bundle.format_version
            ));
        }
        if bundle.archive_version != self.version.archive
            || bundle.license_version != self.version.license
        {
            return Err(format!(
                "bundle uses archive/license {}/{}, target uses {}/{}",
                bundle.archive_version,
                bundle.license_version,
                self.version.archive,
                self.version.license
            ));
        }
        let mut bytes = self.data.clone();
        let mut package = Package::from_wire_bytes(&self.path, bytes)?;
        for dependency in &bundle.dependencies {
            if package.object_wire(&dependency.full_name).is_ok() {
                let existing = package.find_export(&dependency.full_name)?;
                let existing = &package.dump().exports[existing];
                if portable_export(existing)? == portable_export(dependency)? {
                    continue;
                }
                return Err(format!(
                    "dependency {} already exists with different data",
                    dependency.full_name
                ));
            }
            bytes = package.add_single_export(bundle, dependency, None)?;
            package = Package::from_wire_bytes(&self.path, bytes)?;
        }
        package.add_single_export(bundle, &bundle.export, name_override)
    }

    fn add_single_export(
        &self,
        bundle: &ObjectBundle,
        source_export: &ExportDump,
        name_override: Option<&str>,
    ) -> Result<Vec<u8>, String> {
        let mut names = self.names.clone();
        let mut imports = self.imports.clone();
        let mut paths: Vec<String> = self
            .dump()
            .imports
            .into_iter()
            .map(|item| item.full_name)
            .collect();
        for source in &bundle.imports {
            if paths
                .iter()
                .any(|path| path.eq_ignore_ascii_case(&source.full_name))
            {
                continue;
            }
            ensure_import_path(
                &mut names,
                &mut imports,
                &mut paths,
                &source.full_name,
                &source.class,
            )?;
        }
        for source in &bundle.names {
            ensure_name(&mut names, &source.value, source.flags.as_str())?;
        }

        let mut export_dump: ExportDump = clone_json(source_export)?;
        if let Some(name) = name_override {
            export_dump.name = name.into();
            export_dump.full_name = name.into();
        }
        if self
            .dump()
            .exports
            .iter()
            .any(|item| item.full_name.eq_ignore_ascii_case(&export_dump.full_name))
        {
            return Err(format!("export {:?} already exists", export_dump.full_name));
        }

        let class = resolve_path(&imports, &names, &self.exports, &export_dump.class)?;
        let super_class = resolve_path(&imports, &names, &self.exports, &export_dump.super_class)?;
        let outer = if export_dump.outer == 0 {
            ObjectRef::Null
        } else {
            let suffix = format!(".{}", source_export.name);
            let outer_path = source_export
                .full_name
                .strip_suffix(&suffix)
                .ok_or("cannot determine bundled export outer path")?;
            resolve_path(&imports, &names, &self.exports, outer_path)?
        };
        let name_index = ensure_name(&mut names, &export_dump.name, "0x00070010")?;
        let mut exports = self.exports.clone();
        exports.push(Export {
            class: compact_object_ref(class, imports.len(), exports.len() + 1, "new export class")?,
            super_class: compact_object_ref(
                super_class,
                imports.len(),
                exports.len() + 1,
                "new export superclass",
            )?,
            outer,
            name: compact_name_ref(name_index, names.len(), "new export name")?,
            flags: parse_flags(&export_dump.flags)?,
            size: 0,
            offset: 0,
        });

        let mut package = Package {
            path: self.path.clone(),
            data: self.data.clone(),
            version: self.version,
            flags: self.flags,
            names: names.clone(),
            imports: imports.clone(),
            exports: exports.clone(),
        };
        let offset = package.data.len();
        let payload = package.encode_export(&export_dump, None, offset)?;
        package.data.extend_from_slice(&payload);
        let last = package.exports.last_mut().unwrap();
        last.offset = offset;
        last.size = payload.len();
        package.rebuild_tables(
            package.data.clone(),
            names,
            imports,
            package.exports.clone(),
        )
    }

    pub fn remove_export(&self, selector: &str) -> Result<Vec<u8>, String> {
        let index = self.find_export(selector)?;
        let target = self.object_path(ObjectRef::Export(crate::model::ExportIndex::from_usize(
            index,
        )));
        self.ensure_unreferenced(&target, Some(index), None)?;
        let mut names = self.names.clone();
        let none = ensure_name(&mut names, "None", "0x00070010")?;
        let mut exports = self.exports.clone();
        exports[index] = Export {
            class: compact_object_ref(
                ObjectRef::Null,
                self.imports.len(),
                exports.len(),
                "tombstone class",
            )?,
            super_class: compact_object_ref(
                ObjectRef::Null,
                self.imports.len(),
                exports.len(),
                "tombstone superclass",
            )?,
            outer: ObjectRef::Null,
            name: compact_name_ref(none, names.len(), "tombstone name")?,
            flags: 0,
            size: 0,
            offset: 0,
        };
        self.rebuild_tables(self.data.clone(), names, self.imports.clone(), exports)
    }

    pub fn remove_import(&self, selector: &str) -> Result<Vec<u8>, String> {
        let index = self.find_import(selector)?;
        let target = self.object_path(ObjectRef::Import(crate::model::ImportIndex::from_usize(
            index,
        )));
        self.ensure_unreferenced(&target, None, Some(index))?;
        let mut names = self.names.clone();
        let core = ensure_name(&mut names, "Core", "0x00070010")?;
        let package_name = ensure_name(&mut names, "Package", "0x00070010")?;
        let none = ensure_name(&mut names, "None", "0x00070010")?;
        let mut imports = self.imports.clone();
        imports[index] = Import {
            class_package: compact_name_ref(core, names.len(), "tombstone class package")?,
            class_name: compact_name_ref(package_name, names.len(), "tombstone class")?,
            outer: ObjectRef::Null,
            name: compact_name_ref(none, names.len(), "tombstone name")?,
        };
        self.rebuild_tables(self.data.clone(), names, imports, self.exports.clone())
    }

    fn find_export(&self, selector: &str) -> Result<usize, String> {
        find_dump_object(
            selector,
            &self.dump().exports,
            |item| item.reference,
            |item| (&item.name, &item.full_name),
            "export",
        )
    }

    fn find_import(&self, selector: &str) -> Result<usize, String> {
        find_dump_object(
            selector,
            &self.dump().imports,
            |item| item.reference,
            |item| (&item.name, &item.full_name),
            "import",
        )
    }

    fn ensure_unreferenced(
        &self,
        target: &str,
        excluded_export: Option<usize>,
        excluded_import: Option<usize>,
    ) -> Result<(), String> {
        let dump = self.dump();
        for (index, import) in self.imports.iter().enumerate() {
            if Some(index) == excluded_import {
                continue;
            }
            if import.outer != ObjectRef::Null
                && self.object_path(import.outer).eq_ignore_ascii_case(target)
            {
                return Err(format!(
                    "{target} is referenced by import {}",
                    dump.imports[index].full_name
                ));
            }
        }
        for (index, export) in self.exports.iter().enumerate() {
            if Some(index) == excluded_export {
                continue;
            }
            for (field, reference) in [
                ("class", export.class.reference),
                ("superclass", export.super_class.reference),
                ("outer", export.outer),
            ] {
                if reference != ObjectRef::Null
                    && self.object_path(reference).eq_ignore_ascii_case(target)
                {
                    return Err(format!(
                        "{target} is the {field} of export {}",
                        dump.exports[index].full_name
                    ));
                }
            }
            let item = &dump.exports[index];
            if item.raw_data.is_some() || !item.unread_data.is_empty() {
                return Err(format!(
                    "cannot prove {target} is unreferenced: export {} contains opaque bytes",
                    item.full_name
                ));
            }
            if item
                .properties
                .iter()
                .any(|property| property.property_type == "Array" && property.value.is_empty())
            {
                return Err(format!(
                    "cannot prove {target} is unreferenced: export {} has an opaque array property",
                    item.full_name
                ));
            }
            let value = serde_json::to_value(item).map_err(|error| error.to_string())?;
            if json_contains_string(&value, target) {
                return Err(format!(
                    "{target} is referenced by export {}",
                    item.full_name
                ));
            }
        }
        Ok(())
    }

    fn rebuild_tables(
        &self,
        mut data: Vec<u8>,
        names: Vec<Name>,
        imports: Vec<Import>,
        exports: Vec<Export>,
    ) -> Result<Vec<u8>, String> {
        let name_offset = data.len();
        for name in &names {
            write_unreal_string(&mut data, &name.value)?;
            data.extend_from_slice(&name.flags.to_le_bytes());
        }
        let import_offset = data.len();
        for import in &imports {
            let mut writer = BinaryWriter::default();
            writer.compact(import.class_package.index.get() as i32);
            writer.compact(import.class_name.index.get() as i32);
            writer.i32(import.outer.wire());
            writer.compact(import.name.index.get() as i32);
            data.extend_from_slice(&writer.into_bytes());
        }
        let export_offset = data.len();
        for export in &exports {
            let mut writer = BinaryWriter::default();
            writer.compact(export.class.reference.wire());
            writer.compact(export.super_class.reference.wire());
            writer.i32(export.outer.wire());
            writer.compact(export.name.index.get() as i32);
            writer.u32(export.flags);
            writer.compact(i32::try_from(export.size).map_err(|_| "export size exceeds i32")?);
            writer.compact(i32::try_from(export.offset).map_err(|_| "export offset exceeds i32")?);
            data.extend_from_slice(&writer.into_bytes());
        }
        patch_header(&mut data, 12, names.len(), name_offset)?;
        patch_header(&mut data, 20, exports.len(), export_offset)?;
        patch_header(&mut data, 28, imports.len(), import_offset)?;
        Ok(data)
    }

    pub fn export_count(&self) -> usize {
        self.exports.len()
    }

    pub fn dump(&self) -> PackageDump {
        PackageDump {
            file: self.path.display().to_string(),
            version: self.version.archive,
            license: self.version.license,
            flags: format!("0x{:08x}", self.flags),
            names: self
                .names
                .iter()
                .enumerate()
                .map(|(index, name)| NameDump {
                    index,
                    flags: format!("0x{:08x}", name.flags),
                    value: name.value.clone(),
                })
                .collect(),
            imports: self
                .imports
                .iter()
                .enumerate()
                .map(|(index, item)| ImportDump {
                    index,
                    reference: -(index as i32 + 1),
                    name: self.names[item.name.index.get()].value.clone(),
                    class: format!(
                        "{}.{}",
                        self.names[item.class_package.index.get()].value,
                        self.names[item.class_name.index.get()].value
                    ),
                    outer: item.outer.wire(),
                    full_name: self.object_path(ObjectRef::Import(
                        crate::model::ImportIndex::from_usize(index),
                    )),
                })
                .collect(),
            exports: self
                .exports
                .iter()
                .enumerate()
                .map(|(index, item)| self.export_dump(index, item))
                .collect(),
        }
    }

    fn export_dump(&self, index: usize, export: &Export) -> ExportDump {
        let mut dump = ExportDump {
            index,
            reference: index as i32 + 1,
            name: self.names[export.name.index.get()].value.clone(),
            full_name: self.object_path(ObjectRef::Export(crate::model::ExportIndex::from_usize(
                index,
            ))),
            class: self.object_path(export.class.reference),
            super_class: self.object_path(export.super_class.reference),
            outer: export.outer.wire(),
            flags: format!("0x{:08x}", export.flags),
            size: export.size,
            offset: export.offset,
            properties: Vec::new(),
            stack: None,
            native_object: None,
            unread_data: String::new(),
            raw_data: None,
        };
        match self.properties(export) {
            Ok((properties, body_start, stack)) => {
                dump.stack = stack;
                dump.properties = properties
                    .into_iter()
                    .map(|p| PropertyDump {
                        name: p.name,
                        property_type: p.kind,
                        array_index: p.array_index,
                        size: p.raw.len(),
                        value: p.value,
                        raw: hex(&p.raw),
                        wire_header: hex(&p.wire_header),
                    })
                    .collect();
                let end = export.offset + export.size;
                let class = self.object_path(export.class.reference);
                let context = NativeContext {
                    version: self.version,
                    imports: self.imports.len(),
                    exports: self.exports.len(),
                    object_offset: body_start,
                    resolve_name: &|index| {
                        usize::try_from(index)
                            .ok()
                            .and_then(|index| self.names.get(index))
                            .map(|name| name.value.clone())
                            .unwrap_or_else(|| format!("#{index}"))
                    },
                    resolve: &|reference| self.object_path(reference),
                };
                let native =
                    NativeObjectRegistry::decode(&class, &self.data[body_start..end], &context)
                        .ok();
                let consumed = native.as_ref().map(|decoded| decoded.consumed).unwrap_or(0);
                dump.native_object = native.and_then(|decoded| match decoded.object {
                    NativeObject::Unknown => None,
                    object => Some(object),
                });
                dump.unread_data = hex(&self.data[body_start + consumed..end]);
            }
            Err(_) => {
                dump.raw_data = Some(hex(&self.data[export.offset..export.offset + export.size]))
            }
        }
        dump
    }

    fn properties(
        &self,
        export: &Export,
    ) -> Result<(Vec<TaggedProperty>, usize, Option<ObjectStackDump>), String> {
        let mut c = Cursor::new(&self.data, export.offset, export.offset + export.size);
        let owner_class = self.object_path(export.class.reference);
        let stack = if export.flags & HAS_STACK != 0 {
            let node = c.compact()?;
            let state_node = c.compact()?;
            let probe_mask_low = c.u32()?;
            let probe_mask_high = c.u32()?;
            Some(ObjectStackDump {
                node,
                state_node,
                probe_mask: (u64::from(probe_mask_high) << 32) | u64::from(probe_mask_low),
                latent_action: c.i32()?,
                state_offset: c.compact()?,
            })
        } else {
            None
        };
        let mut result = Vec::new();
        loop {
            let property_start = c.pos;
            let name = self.names[index(c.compact()?, self.names.len(), "property name")?]
                .value
                .clone();
            if name == "None" {
                return Ok((result, c.pos, stack));
            }
            let info = c.u8()?;
            let kind = info & 15;
            let struct_name = if kind == 10 {
                Some(
                    self.names[index(c.compact()?, self.names.len(), "struct name")?]
                        .value
                        .clone(),
                )
            } else {
                None
            };
            let size = match (info >> 4) & 7 {
                0 => 1,
                1 => 2,
                2 => 4,
                3 => 12,
                4 => 16,
                5 => c.u8()? as usize,
                6 => c.u16()? as usize,
                7 => c.u32()? as usize,
                _ => unreachable!(),
            };
            let array_index = if info & 0x80 != 0 && kind != 3 {
                c.compact()?
            } else {
                0
            };
            let raw_start = c.pos;
            let raw = c.take(size)?.to_vec();
            let type_name = struct_name
                .as_deref()
                .unwrap_or(type_name(kind))
                .to_string();
            let value = decode_value(
                info,
                struct_name.as_deref(),
                &name,
                &owner_class,
                &raw,
                self,
            );
            result.push(TaggedProperty {
                name,
                kind: type_name,
                array_index,
                raw,
                value,
                wire_header: self.data[property_start..raw_start].to_vec(),
            });
        }
    }

    fn object_path(&self, mut reference: ObjectRef) -> String {
        if reference == ObjectRef::Null {
            return String::new();
        }
        let mut parts = Vec::new();
        for _ in 0..128 {
            if let ObjectRef::Export(index) = reference {
                let Some(e) = self.exports.get(index.get()) else {
                    break;
                };
                parts.push(self.names[e.name.index.get()].value.clone());
                reference = e.outer;
            } else if let ObjectRef::Import(index) = reference {
                let Some(e) = self.imports.get(index.get()) else {
                    break;
                };
                parts.push(self.names[e.name.index.get()].value.clone());
                reference = e.outer;
            } else {
                break;
            }
        }
        parts.reverse();
        parts.join(".")
    }
}

struct TaggedProperty {
    name: String,
    kind: String,
    array_index: i32,
    raw: Vec<u8>,
    value: String,
    wire_header: Vec<u8>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DumpDocument {
    pub packages: Vec<PackageDump>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PackageDump {
    pub file: String,
    pub version: u16,
    pub license: u16,
    pub flags: String,
    pub names: Vec<NameDump>,
    pub imports: Vec<ImportDump>,
    pub exports: Vec<ExportDump>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NameDump {
    pub index: usize,
    pub flags: String,
    pub value: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportDump {
    pub index: usize,
    pub reference: i32,
    pub name: String,
    pub class: String,
    pub outer: i32,
    pub full_name: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportDump {
    pub index: usize,
    pub reference: i32,
    pub name: String,
    pub full_name: String,
    pub class: String,
    pub super_class: String,
    pub outer: i32,
    pub flags: String,
    pub size: usize,
    pub offset: usize,
    pub properties: Vec<PropertyDump>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stack: Option<ObjectStackDump>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub native_object: Option<NativeObject>,
    pub unread_data: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_data: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ObjectStackDump {
    pub node: i32,
    pub state_node: i32,
    pub probe_mask: u64,
    pub latent_action: i32,
    pub state_offset: i32,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PropertyDump {
    pub name: String,
    #[serde(rename = "type")]
    pub property_type: String,
    pub array_index: i32,
    pub size: usize,
    pub value: String,
    pub raw: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub wire_header: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ObjectBundle {
    pub format_version: u32,
    pub archive_version: u16,
    pub license_version: u16,
    pub names: Vec<NameDump>,
    pub imports: Vec<ImportDump>,
    pub dependencies: Vec<ExportDump>,
    pub export: ExportDump,
}

fn decode_value(
    info: u8,
    structure: Option<&str>,
    property_name: &str,
    owner_class: &str,
    raw: &[u8],
    package: &Package,
) -> String {
    match (info & 15, raw.len()) {
        (1, 1) => raw[0].to_string(),
        (2, 4) => i32::from_le_bytes(raw.try_into().unwrap()).to_string(),
        // A boolean's value rides in the property tag, not in its data.
        (3, _) => bool_value(info),
        (4, 4) => f32::from_le_bytes(raw.try_into().unwrap()).to_string(),
        (5 | 6, _) => Cursor::new(raw, 0, raw.len())
            .compact()
            .ok()
            .map(|r| {
                if info & 15 == 5 {
                    ObjectRef::checked(
                        r,
                        package.imports.len(),
                        package.exports.len(),
                        "property object",
                    )
                    .map(|reference| package.object_path(reference))
                    .unwrap_or_default()
                } else {
                    package
                        .names
                        .get(r as usize)
                        .map(|n| n.value.clone())
                        .unwrap_or_else(|| r.to_string())
                }
            })
            .unwrap_or_default(),
        (10, 12) if structure.is_some_and(|s| s.eq_ignore_ascii_case("Vector")) => {
            format!("{},{},{}", f32_at(raw, 0), f32_at(raw, 4), f32_at(raw, 8))
        }
        (10, 12) if structure.is_some_and(|s| s.eq_ignore_ascii_case("Rotator")) => {
            format!("{},{},{}", i32_at(raw, 0), i32_at(raw, 4), i32_at(raw, 8))
        }
        // Unreal stores FColor channels in blue, green, red, alpha order.
        (10, 4) if structure.is_some_and(|s| s.eq_ignore_ascii_case("Color")) => {
            format!("{},{},{},{}", raw[2], raw[1], raw[0], raw[3])
        }
        (10, _)
            if structure.is_some_and(|s| {
                s.eq_ignore_ascii_case("TerrainLayer")
                    || s.eq_ignore_ascii_case("TextureModifyinfo")
                    || s.eq_ignore_ascii_case("Range")
                    || s.eq_ignore_ascii_case("RangeVector")
            }) =>
        {
            decode_tagged_struct(raw, package).unwrap_or_default()
        }
        (12, 12) => format!("{},{},{}", i32_at(raw, 0), i32_at(raw, 4), i32_at(raw, 8)),
        (9, _) if owner_class == "Engine.AnimNotify_RandomSound" && property_name == "Sounds" => {
            decode_object_array(
                raw,
                package.imports.len(),
                package.exports.len(),
                &|reference| package.object_path(reference),
            )
            .unwrap_or_default()
        }
        // An actor's skins are the materials it is drawn with.
        (9, _) if property_name == "Skins" => decode_object_array(
            raw,
            package.imports.len(),
            package.exports.len(),
            &|reference| package.object_path(reference),
        )
        .unwrap_or_default(),
        (9, _) if owner_class == "Engine.StaticMesh" && property_name == "Materials" => {
            decode_static_mesh_materials(raw, package).unwrap_or_default()
        }
        (9, _) if owner_class == "Engine.Emitter" && property_name == "Emitters" => {
            decode_object_array(
                raw,
                package.imports.len(),
                package.exports.len(),
                &|reference| package.object_path(reference),
            )
            .unwrap_or_default()
        }
        (9, _)
            if owner_class.ends_with("Emitter")
                && matches!(property_name, "ColorScale" | "SizeScale" | "L2GameEvent") =>
        {
            decode_struct_array(raw, package).unwrap_or_default()
        }
        (12, _) => String::from_utf8_lossy(raw.strip_suffix(&[0]).unwrap_or(raw)).into_owned(),
        _ => String::new(),
    }
}

fn bool_value(info: u8) -> String {
    if info & 0x80 != 0 { "true" } else { "false" }.into()
}

fn decode_tagged_struct(raw: &[u8], package: &Package) -> Result<String, String> {
    let mut cursor = Cursor::new(raw, 0, raw.len());
    let fields = decode_tagged_fields(&mut cursor, package)?;
    if cursor.pos != raw.len() {
        return Err("tagged struct has trailing bytes".into());
    }
    serde_json::to_string(&fields).map_err(|error| error.to_string())
}

fn decode_struct_array(raw: &[u8], package: &Package) -> Result<String, String> {
    let mut cursor = Cursor::new(raw, 0, raw.len());
    let count = cursor.nonnegative_compact("struct array length")?;
    let mut elements = Vec::with_capacity(count);
    for _ in 0..count {
        elements.push(decode_tagged_fields(&mut cursor, package)?);
    }
    if cursor.pos != raw.len() {
        return Err("struct array has trailing bytes".into());
    }
    serde_json::to_string(&elements).map_err(|error| error.to_string())
}

fn decode_tagged_fields(
    cursor: &mut Cursor<'_>,
    package: &Package,
) -> Result<serde_json::Map<String, serde_json::Value>, String> {
    let mut fields = serde_json::Map::new();
    loop {
        let name_index = index(
            cursor.compact()?,
            package.names.len(),
            "struct property name",
        )?;
        let name = &package.names[name_index].value;
        if name.eq_ignore_ascii_case("None") {
            break;
        }
        let info = cursor.u8()?;
        let kind = info & 15;
        let structure = if kind == 10 {
            let structure_index =
                index(cursor.compact()?, package.names.len(), "nested struct type")?;
            Some(package.names[structure_index].value.as_str())
        } else {
            None
        };
        let size = match (info >> 4) & 7 {
            0 => 1,
            1 => 2,
            2 => 4,
            3 => 12,
            4 => 16,
            5 => cursor.u8()? as usize,
            6 => cursor.u16()? as usize,
            7 => cursor.u32()? as usize,
            _ => unreachable!(),
        };
        let array_index = if info & 0x80 != 0 && kind != 3 {
            cursor.compact()?
        } else {
            0
        };
        let value = cursor.take(size)?;
        let decoded = decode_value(info, structure, name, "", value, package);
        let key = if array_index == 0 {
            name.clone()
        } else {
            format!("{name}[{array_index}]")
        };
        fields.insert(key, nested_field(decoded));
    }
    Ok(fields)
}

// A nested struct decodes to its own JSON document. Reparsing it keeps the
// enclosing struct a single document instead of a string holding one.
fn nested_field(decoded: String) -> serde_json::Value {
    serde_json::from_str::<serde_json::Value>(&decoded)
        .ok()
        .filter(|value| value.is_object() || value.is_array())
        .unwrap_or(serde_json::Value::String(decoded))
}

fn decode_static_mesh_materials(raw: &[u8], package: &Package) -> Result<String, String> {
    let mut cursor = Cursor::new(raw, 0, raw.len());
    let count = cursor.nonnegative_compact("static mesh material count")?;
    let mut materials = Vec::with_capacity(count);
    for _ in 0..count {
        let mut material = String::new();
        loop {
            let name_index = index(
                cursor.compact()?,
                package.names.len(),
                "static mesh material property name",
            )?;
            let name = &package.names[name_index].value;
            if name.eq_ignore_ascii_case("None") {
                break;
            }
            let info = cursor.u8()?;
            let kind = info & 15;
            if kind == 10 {
                cursor.compact()?;
            }
            let size = match (info >> 4) & 7 {
                0 => 1,
                1 => 2,
                2 => 4,
                3 => 12,
                4 => 16,
                5 => cursor.u8()? as usize,
                6 => cursor.u16()? as usize,
                7 => cursor.u32()? as usize,
                _ => unreachable!(),
            };
            if info & 0x80 != 0 && kind != 3 {
                cursor.compact()?;
            }
            let value = cursor.take(size)?;
            if name.eq_ignore_ascii_case("Material") && kind == 5 {
                let mut value_cursor = Cursor::new(value, 0, value.len());
                let reference = ObjectRef::checked(
                    value_cursor.compact()?,
                    package.imports.len(),
                    package.exports.len(),
                    "static mesh material",
                )?;
                material = package.object_path(reference);
            }
        }
        materials.push(material);
    }
    if cursor.pos != raw.len() {
        return Err("static mesh materials have trailing bytes".into());
    }
    serde_json::to_string(&materials).map_err(|error| error.to_string())
}

fn decode_object_array(
    raw: &[u8],
    imports: usize,
    exports: usize,
    resolve: &dyn Fn(ObjectRef) -> String,
) -> Result<String, String> {
    let mut cursor = Cursor::new(raw, 0, raw.len());
    let count = cursor.nonnegative_compact("array length")?;
    let mut values = Vec::with_capacity(count);
    for _ in 0..count {
        let wire = cursor.compact()?;
        let reference = ObjectRef::checked(wire, imports, exports, "array object")?;
        values.push(resolve(reference));
    }
    if cursor.pos != raw.len() {
        return Err("object array has trailing bytes".into());
    }
    serde_json::to_string(&values).map_err(|error| error.to_string())
}

fn f32_at(raw: &[u8], at: usize) -> f32 {
    f32::from_le_bytes(raw[at..at + 4].try_into().unwrap())
}

fn i32_at(raw: &[u8], at: usize) -> i32 {
    i32::from_le_bytes(raw[at..at + 4].try_into().unwrap())
}
fn type_name(kind: u8) -> &'static str {
    [
        "None",
        "Byte",
        "Int",
        "Bool",
        "Float",
        "Object",
        "Name",
        "Delegate",
        "Class",
        "Array",
        "Struct",
        "Vector",
        "Rotator",
        "String",
        "Map",
        "FixedArray",
    ]
    .get(kind as usize)
    .copied()
    .unwrap_or("Unknown")
}
fn hex(bytes: &[u8]) -> String {
    const H: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(H[(b >> 4) as usize] as char);
        s.push(H[(b & 15) as usize] as char);
    }
    s
}

fn decode_hex(value: &str) -> Result<Vec<u8>, String> {
    if !value.len().is_multiple_of(2) {
        return Err("hex value has an odd number of digits".into());
    }
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let text = std::str::from_utf8(pair).map_err(|error| error.to_string())?;
            u8::from_str_radix(text, 16).map_err(|error| error.to_string())
        })
        .collect()
}

fn ensure_name(names: &mut Vec<Name>, value: &str, flags: &str) -> Result<usize, String> {
    if let Some(index) = names
        .iter()
        .position(|name| name.value.eq_ignore_ascii_case(value))
    {
        return Ok(index);
    }
    names.push(Name {
        value: value.into(),
        flags: parse_flags(flags)?,
    });
    Ok(names.len() - 1)
}

fn parse_flags(value: &str) -> Result<u32, String> {
    let value = value.strip_prefix("0x").unwrap_or(value);
    u32::from_str_radix(value, 16).map_err(|error| format!("invalid flags {value:?}: {error}"))
}

fn compact_name_ref(index: usize, names: usize, field: &str) -> Result<CompactNameRef, String> {
    CompactNameRef::checked(
        CompactInt::new(i32::try_from(index).map_err(|_| "name index exceeds i32")?),
        names,
        field,
    )
}

fn compact_object_ref(
    reference: ObjectRef,
    imports: usize,
    exports: usize,
    field: &str,
) -> Result<CompactObjectRef, String> {
    CompactObjectRef::checked(CompactInt::new(reference.wire()), imports, exports, field)
}

fn ensure_import_path(
    names: &mut Vec<Name>,
    imports: &mut Vec<Import>,
    paths: &mut Vec<String>,
    path: &str,
    class: &str,
) -> Result<ObjectRef, String> {
    let components: Vec<_> = path.split('.').filter(|value| !value.is_empty()).collect();
    if components.is_empty() {
        return Err("import path is empty".into());
    }
    let mut outer = ObjectRef::Null;
    let mut prefix = String::new();
    for (index, component) in components.iter().enumerate() {
        if !prefix.is_empty() {
            prefix.push('.');
        }
        prefix.push_str(component);
        if let Some(existing) = paths
            .iter()
            .position(|value| value.eq_ignore_ascii_case(&prefix))
        {
            outer = ObjectRef::Import(crate::model::ImportIndex::from_usize(existing));
            continue;
        }
        let entry_class = if index + 1 == components.len() {
            class
        } else {
            "Core.Package"
        };
        let (class_package, class_name) = entry_class
            .split_once('.')
            .ok_or_else(|| format!("import class {entry_class:?} must contain a package"))?;
        let class_package = ensure_name(names, class_package, "0x00070010")?;
        let class_name = ensure_name(names, class_name, "0x00070010")?;
        let name = ensure_name(names, component, "0x00070010")?;
        imports.push(Import {
            class_package: compact_name_ref(class_package, names.len(), "import class package")?,
            class_name: compact_name_ref(class_name, names.len(), "import class")?,
            outer,
            name: compact_name_ref(name, names.len(), "import name")?,
        });
        paths.push(prefix.clone());
        outer = ObjectRef::Import(crate::model::ImportIndex::from_usize(imports.len() - 1));
    }
    Ok(outer)
}

fn resolve_path(
    imports: &[Import],
    names: &[Name],
    exports: &[Export],
    path: &str,
) -> Result<ObjectRef, String> {
    if path.is_empty() {
        return Ok(ObjectRef::Null);
    }
    for index in 0..imports.len() {
        let reference = ObjectRef::Import(crate::model::ImportIndex::from_usize(index));
        if object_path_from(reference, imports, names, exports).eq_ignore_ascii_case(path) {
            return Ok(reference);
        }
    }
    for index in 0..exports.len() {
        let reference = ObjectRef::Export(crate::model::ExportIndex::from_usize(index));
        if object_path_from(reference, imports, names, exports).eq_ignore_ascii_case(path) {
            return Ok(reference);
        }
    }
    Err(format!(
        "object {path:?} is not available in the target package"
    ))
}

fn object_path_from(
    reference: ObjectRef,
    imports: &[Import],
    names: &[Name],
    exports: &[Export],
) -> String {
    let (name, outer) = match reference {
        ObjectRef::Null => return String::new(),
        ObjectRef::Import(index) => {
            let item = &imports[index.get()];
            (&names[item.name.index.get()].value, item.outer)
        }
        ObjectRef::Export(index) => {
            let item = &exports[index.get()];
            (&names[item.name.index.get()].value, item.outer)
        }
    };
    let outer = object_path_from(outer, imports, names, exports);
    if outer.is_empty() {
        name.clone()
    } else {
        format!("{outer}.{name}")
    }
}

fn find_dump_object<T>(
    selector: &str,
    items: &[T],
    reference: impl Fn(&T) -> i32,
    names: impl Fn(&T) -> (&str, &str),
    kind: &str,
) -> Result<usize, String> {
    if let Some(value) = selector.strip_prefix('#') {
        let wire = value
            .parse::<i32>()
            .map_err(|error| format!("invalid reference {selector:?}: {error}"))?;
        return items
            .iter()
            .position(|item| reference(item) == wire)
            .ok_or_else(|| format!("no {kind} has reference {wire}"));
    }
    let matches: Vec<_> = items
        .iter()
        .enumerate()
        .filter(|(_, item)| {
            let (name, full_name) = names(item);
            name.eq_ignore_ascii_case(selector) || full_name.eq_ignore_ascii_case(selector)
        })
        .map(|(index, _)| index)
        .collect();
    match matches.as_slice() {
        [index] => Ok(*index),
        [] => Err(format!("{kind} {selector:?} was not found")),
        _ => Err(format!(
            "{kind} name {selector:?} is ambiguous; use its full path or reference"
        )),
    }
}

fn json_contains_string(value: &serde_json::Value, target: &str) -> bool {
    match value {
        serde_json::Value::String(value) => value.eq_ignore_ascii_case(target),
        serde_json::Value::Array(values) => values
            .iter()
            .any(|value| json_contains_string(value, target)),
        serde_json::Value::Object(values) => values
            .values()
            .any(|value| json_contains_string(value, target)),
        _ => false,
    }
}

fn collect_export_dependencies(
    index: usize,
    exports: &[ExportDump],
    selected: &mut Vec<usize>,
) -> Result<(), String> {
    if selected.contains(&index) {
        return Ok(());
    }
    selected.push(index);
    let value = serde_json::to_value(&exports[index]).map_err(|error| error.to_string())?;
    for (dependency, candidate) in exports.iter().enumerate() {
        if dependency != index && json_contains_string(&value, &candidate.full_name) {
            collect_export_dependencies(dependency, exports, selected)?;
        }
    }
    let position = selected.iter().position(|value| *value == index).unwrap();
    let index = selected.remove(position);
    selected.push(index);
    Ok(())
}

fn collect_import_outers(index: usize, imports: &[ImportDump], selected: &mut Vec<usize>) {
    if selected.contains(&index) {
        return;
    }
    if imports[index].outer < 0 {
        let outer = imports[index].outer.unsigned_abs() as usize - 1;
        collect_import_outers(outer, imports, selected);
    }
    selected.push(index);
}

fn clone_json<T>(value: &T) -> Result<T, String>
where
    T: Serialize + for<'de> Deserialize<'de>,
{
    serde_json::from_value(serde_json::to_value(value).map_err(|error| error.to_string())?)
        .map_err(|error| error.to_string())
}

fn write_unreal_string(output: &mut Vec<u8>, value: &str) -> Result<(), String> {
    let mut writer = BinaryWriter::default();
    if value.is_ascii() {
        let length = value.len().checked_add(1).ok_or("string length overflow")?;
        writer.compact(i32::try_from(length).map_err(|_| "string length exceeds i32")?);
        writer.bytes(value.as_bytes());
        writer.u8(0);
    } else {
        let mut units: Vec<_> = value.encode_utf16().collect();
        units.push(0);
        let length = i32::try_from(units.len()).map_err(|_| "string length exceeds i32")?;
        writer.compact(-length);
        for unit in units {
            writer.u16(unit);
        }
    }
    output.extend_from_slice(&writer.into_bytes());
    Ok(())
}

fn patch_header(
    data: &mut [u8],
    position: usize,
    count: usize,
    offset: usize,
) -> Result<(), String> {
    let count = u32::try_from(count).map_err(|_| "table count exceeds u32")?;
    let offset = u32::try_from(offset).map_err(|_| "table offset exceeds u32")?;
    data[position..position + 4].copy_from_slice(&count.to_le_bytes());
    data[position + 4..position + 8].copy_from_slice(&offset.to_le_bytes());
    Ok(())
}

fn semantic_export(export: &ExportDump) -> Result<serde_json::Value, String> {
    serde_json::to_value((
        &export.stack,
        &export.properties,
        &export.native_object,
        &export.unread_data,
        &export.raw_data,
    ))
    .map_err(|error| error.to_string())
}

fn portable_export(export: &ExportDump) -> Result<serde_json::Value, String> {
    let mut value = serde_json::to_value((
        &export.name,
        &export.full_name,
        &export.class,
        &export.super_class,
        &export.flags,
        &export.properties,
        &export.native_object,
        &export.unread_data,
        &export.raw_data,
    ))
    .map_err(|error| error.to_string())?;
    remove_provenance(&mut value);
    Ok(value)
}

fn remove_provenance(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Array(values) => values.iter_mut().for_each(remove_provenance),
        serde_json::Value::Object(values) => {
            for key in [
                "raw",
                "wireHeader",
                "wireValue",
                "reference",
                "serializedEndOffset",
                "movesEndOffset",
            ] {
                values.remove(key);
            }
            values.values_mut().for_each(remove_provenance);
        }
        _ => {}
    }
}

fn property_kind(value: &str) -> Result<(u8, Option<&str>), String> {
    let kind = match value {
        "Byte" => (1, None),
        "Int" => (2, None),
        "Bool" => (3, None),
        "Float" => (4, None),
        "Object" => (5, None),
        "Name" => (6, None),
        "Delegate" => (7, None),
        "Class" => (8, None),
        "Array" => (9, None),
        "Vector" => (11, None),
        "Rotator" => (12, None),
        "String" => (13, None),
        "Map" => (14, None),
        "FixedArray" => (15, None),
        structure if !structure.is_empty() => (10, Some(structure)),
        _ => return Err("property type is empty".into()),
    };
    Ok(kind)
}

fn encode_triplet(value: &str, floats: bool) -> Result<Vec<u8>, String> {
    let parts: Vec<_> = value.split(',').map(str::trim).collect();
    if parts.len() != 3 {
        return Err(format!(
            "expected three comma-separated values, got {value:?}"
        ));
    }
    let mut bytes = Vec::with_capacity(12);
    for part in parts {
        if floats {
            bytes.extend_from_slice(
                &part
                    .parse::<f32>()
                    .map_err(|error| error.to_string())?
                    .to_le_bytes(),
            );
        } else {
            bytes.extend_from_slice(
                &part
                    .parse::<i32>()
                    .map_err(|error| error.to_string())?
                    .to_le_bytes(),
            );
        }
    }
    Ok(bytes)
}

pub fn reencrypt_like(original: &[u8], plain: Vec<u8>, filename: &str) -> Result<Vec<u8>, String> {
    if original.starts_with(&MAGIC.to_le_bytes()) {
        return Ok(plain);
    }
    if original.len() < 28 {
        return Err("wire source has no recognized Unreal package or encryption header".into());
    }
    let header: String = original[..28]
        .chunks_exact(2)
        .map(|bytes| {
            char::from_u32(u16::from_le_bytes([bytes[0], bytes[1]]) as u32).unwrap_or('\0')
        })
        .collect();
    let version = header
        .strip_prefix("Lineage2Ver")
        .ok_or("wire source has an unsupported encryption header")?;
    let key = match version {
        "111" => 0xac,
        "121" => filename
            .to_ascii_lowercase()
            .bytes()
            .fold(0u8, u8::wrapping_add),
        _ => {
            return Err(format!(
                "Lineage II encryption version {version} is not supported"
            ));
        }
    };
    let mut encrypted = original[..28].to_vec();
    encrypted.extend(plain.into_iter().map(|byte| byte ^ key));
    Ok(encrypted)
}

pub fn decrypt(bytes: Vec<u8>, filename: &str) -> Result<Vec<u8>, String> {
    if bytes.starts_with(&MAGIC.to_le_bytes()) {
        return Ok(bytes);
    }
    if bytes.len() < 28 {
        return Ok(bytes);
    }
    let header: String = bytes[..28]
        .chunks_exact(2)
        .map(|b| char::from_u32(u16::from_le_bytes([b[0], b[1]]) as u32).unwrap_or('\0'))
        .collect();
    let Some(version) = header.strip_prefix("Lineage2Ver") else {
        return Ok(bytes);
    };
    let key = match version {
        "111" => 0xac,
        "121" => filename
            .to_ascii_lowercase()
            .bytes()
            .fold(0u8, u8::wrapping_add),
        _ => {
            return Err(format!(
                "Lineage II encryption version {version} is not supported"
            ));
        }
    };
    Ok(bytes[28..].iter().map(|b| b ^ key).collect())
}

pub fn encrypt(bytes: Vec<u8>) -> Result<Vec<u8>, String> {
    if !bytes.starts_with(&MAGIC.to_le_bytes()) {
        return Err("input is not an unencrypted Unreal package".into());
    }
    let mut encrypted: Vec<u8> = "Lineage2Ver111"
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    encrypted.extend(bytes.into_iter().map(|byte| byte ^ 0xac));
    Ok(encrypted)
}

fn index(value: i32, len: usize, what: &str) -> Result<usize, String> {
    usize::try_from(value)
        .ok()
        .filter(|&i| i < len)
        .ok_or_else(|| format!("invalid {what} index {value}"))
}

fn export_error(index: usize, field: &str, error: String) -> String {
    format!("cannot read export {index} {field}: {error}")
}

struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
    end: usize,
}
impl<'a> Cursor<'a> {
    fn new(data: &'a [u8], pos: usize, end: usize) -> Self {
        Self {
            data,
            pos,
            end: end.min(data.len()),
        }
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self.pos.checked_add(n).ok_or("offset overflow")?;
        if end > self.end {
            return Err(format!("unexpected end of data at {}", self.pos));
        }
        let v = &self.data[self.pos..end];
        self.pos = end;
        Ok(v)
    }
    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, String> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn i32(&mut self) -> Result<i32, String> {
        Ok(self.u32()? as i32)
    }
    fn count(&mut self, n: &str) -> Result<usize, String> {
        usize::try_from(self.i32()?).map_err(|_| format!("negative {n} count"))
    }
    fn offset(&mut self, n: &str) -> Result<usize, String> {
        usize::try_from(self.i32()?)
            .ok()
            .filter(|&v| v <= self.data.len())
            .ok_or_else(|| format!("invalid {n} offset"))
    }
    fn nonnegative_compact(&mut self, n: &str) -> Result<usize, String> {
        usize::try_from(self.compact()?).map_err(|_| format!("negative {n}"))
    }
    fn compact(&mut self) -> Result<i32, String> {
        Ok(self.compact_value()?.get())
    }
    fn compact_value(&mut self) -> Result<CompactInt, String> {
        let mut reader = BinaryReader::new(&self.data[self.pos..self.end]);
        let value = CompactInt::read(&mut reader).map_err(|e| format!("codec error: {e:?}"))?;
        self.pos += reader.position();
        Ok(value)
    }
    fn unreal_string(&mut self) -> Result<String, String> {
        let len = self.compact()?;
        if len == 0 {
            return Ok(String::new());
        }
        if len > 0 {
            let raw = self.take(len as usize)?;
            Ok(String::from_utf8_lossy(raw.strip_suffix(&[0]).unwrap_or(raw)).into_owned())
        } else {
            let raw = self.take((-len as usize) * 2)?;
            let units: Vec<_> = raw
                .chunks_exact(2)
                .map(|b| u16::from_le_bytes([b[0], b[1]]))
                .take_while(|&x| x != 0)
                .collect();
            Ok(String::from_utf16_lossy(&units))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotators_decode_as_signed_unreal_units() {
        let raw = [0xc0, 0xf2, 0, 0, 0x44, 0x3c, 0, 0, 0, 0, 0, 0];
        assert_eq!(i32_at(&raw, 0), 62144);
        assert_eq!(i32_at(&raw, 4), 15428);
        assert_eq!(i32_at(&raw, 8), 0);
    }

    #[test]
    fn unencrypted_packages_are_not_modified() {
        let mut bytes = MAGIC.to_le_bytes().to_vec();
        bytes.extend_from_slice(b"plain package bytes");
        assert_eq!(decrypt(bytes.clone(), "animations.ukx"), Ok(bytes));
    }

    #[test]
    fn version_111_encryption_round_trips() {
        let mut plain = MAGIC.to_le_bytes().to_vec();
        plain.extend_from_slice(b"plain package bytes");
        let encrypted = encrypt(plain.clone()).unwrap();
        let expected_header: Vec<u8> = "Lineage2Ver111"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        assert_eq!(&encrypted[..28], expected_header);
        assert_eq!(decrypt(encrypted, "any.extension"), Ok(plain));
    }

    #[test]
    fn sound_object_arrays_resolve_every_reference() {
        let value = decode_object_array(&[2, 1, 2], 0, 2, &|reference| {
            format!("Sound{}", reference.wire())
        });
        assert_eq!(value, Ok(r#"["Sound1","Sound2"]"#.into()));
    }

    #[test]
    fn static_mesh_material_array_resolves_tagged_object_references() {
        let name_ref =
            |index| CompactNameRef::checked(CompactInt::new(index), 4, "test name").unwrap();
        let package = Package {
            path: PathBuf::new(),
            data: Vec::new(),
            version: PackageVersion::new(123, 37),
            flags: 0,
            names: vec![
                Name {
                    value: "None".into(),
                    flags: 0,
                },
                Name {
                    value: "Material".into(),
                    flags: 0,
                },
                Name {
                    value: "TexturePackage".into(),
                    flags: 0,
                },
                Name {
                    value: "Stone".into(),
                    flags: 0,
                },
            ],
            imports: vec![
                Import {
                    class_package: name_ref(0),
                    class_name: name_ref(0),
                    outer: ObjectRef::Null,
                    name: name_ref(2),
                },
                Import {
                    class_package: name_ref(0),
                    class_name: name_ref(0),
                    outer: ObjectRef::Import(crate::model::ImportIndex::from_usize(0)),
                    name: name_ref(3),
                },
            ],
            exports: Vec::new(),
        };
        let raw = [1, 1, 5, 0x82, 0];
        assert_eq!(
            decode_static_mesh_materials(&raw, &package),
            Ok(r#"["TexturePackage.Stone"]"#.into())
        );
    }

    #[test]
    fn boolean_properties_read_their_value_from_the_property_tag() {
        let package = emitter_test_package();
        assert_eq!(
            decode_value(0xd3, None, "FadeOut", "", &[], &package),
            "true"
        );
        assert_eq!(
            decode_value(0x53, None, "FadeOut", "", &[], &package),
            "false"
        );
    }

    #[test]
    fn range_vectors_decode_each_axis() {
        let package = emitter_test_package();
        // X: (Min -300, Max 300), Y and Z left at their defaults.
        let raw = [
            1, 0x5a, 2, 0x0d, 3, 0x24, 0x00, 0x00, 0x96, 0xc3, 4, 0x24, 0x00, 0x00, 0x96, 0x43, 0,
            0,
        ];
        assert_eq!(
            decode_value(
                0x7a,
                Some("RangeVector"),
                "StartSizeRange",
                "",
                &raw,
                &package
            ),
            r#"{"X":{"Max":"300","Min":"-300"}}"#
        );
    }

    #[test]
    fn emitter_colour_scales_decode_as_an_array_of_structs() {
        let package = emitter_test_package();
        // Two keyframes of {RelativeTime, Color}; Color is stored as B, G, R, A.
        let raw = [
            2, 5, 0x24, 0x00, 0x00, 0x00, 0x00, 6, 0x2a, 7, 0x13, 0x0d, 0xfb, 0xff, 0, 5, 0x24,
            0x00, 0x00, 0x80, 0x3f, 6, 0x2a, 7, 0x00, 0x00, 0xff, 0xff, 0,
        ];
        assert_eq!(
            decode_value(
                0x79,
                None,
                "ColorScale",
                "Engine.SpriteEmitter",
                &raw,
                &package
            ),
            r#"[{"Color":"251,13,19,255","RelativeTime":"0"},{"Color":"255,0,0,255","RelativeTime":"1"}]"#
        );
    }

    fn emitter_test_package() -> Package {
        Package {
            path: PathBuf::new(),
            data: Vec::new(),
            version: PackageVersion::new(123, 37),
            flags: 0,
            names: [
                "None",
                "X",
                "Range",
                "Min",
                "Max",
                "RelativeTime",
                "Color",
                "Color",
            ]
            .into_iter()
            .map(|value| Name {
                value: value.into(),
                flags: 0,
            })
            .collect(),
            imports: Vec::new(),
            exports: Vec::new(),
        }
    }

    #[test]
    fn compact_ints() {
        assert_eq!(Cursor::new(&[0x3f], 0, 1).compact().unwrap(), 63);
        assert_eq!(Cursor::new(&[0x40, 1], 0, 2).compact().unwrap(), 64);
        assert_eq!(Cursor::new(&[0xc0, 1], 0, 2).compact().unwrap(), -64);
    }

    #[test]
    fn adding_dotted_import_creates_package_outer_chain() {
        let mut names = Vec::new();
        let mut imports = Vec::new();
        let mut paths = Vec::new();
        let reference = ensure_import_path(
            &mut names,
            &mut imports,
            &mut paths,
            "Audio.Monsters.hit",
            "Engine.Sound",
        )
        .unwrap();

        assert_eq!(reference.wire(), -3);
        assert_eq!(paths, ["Audio", "Audio.Monsters", "Audio.Monsters.hit"]);
        assert_eq!(imports[0].outer, ObjectRef::Null);
        assert_eq!(imports[1].outer.wire(), -1);
        assert_eq!(imports[2].outer.wire(), -2);
        assert_eq!(names[imports[2].class_name.index.get()].value, "Sound");
    }

    #[test]
    fn unreal_string_writer_round_trips_ascii_and_unicode() {
        for value in ["Grim_reaper", "Звук"] {
            let mut bytes = Vec::new();
            write_unreal_string(&mut bytes, value).unwrap();
            assert_eq!(
                Cursor::new(&bytes, 0, bytes.len()).unreal_string().unwrap(),
                value
            );
        }
    }
}
