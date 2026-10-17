use crate::codec::CompactInt;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PackageVersion {
    pub archive: u16,
    pub license: u16,
}

impl PackageVersion {
    pub fn new(archive: u16, license: u16) -> Self {
        Self { archive, license }
    }
    pub fn has_terrain_vertex_colors(self) -> bool {
        self.archive >= 117
    }
    pub fn has_environment_lights(self) -> bool {
        self.license > 3
    }
    pub fn has_static_mesh_unknown_indices(self) -> bool {
        self.license > 11
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct NameIndex(usize);

impl NameIndex {
    pub fn checked(value: i32, len: usize, field: &str) -> Result<Self, String> {
        usize::try_from(value)
            .ok()
            .filter(|&index| index < len)
            .map(Self)
            .ok_or_else(|| format!("invalid {field} index {value}"))
    }
    pub fn get(self) -> usize {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct ImportIndex(usize);

impl ImportIndex {
    pub fn from_usize(value: usize) -> Self {
        Self(value)
    }
    pub fn get(self) -> usize {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct ExportIndex(usize);

impl ExportIndex {
    pub fn from_usize(value: usize) -> Self {
        Self(value)
    }
    pub fn get(self) -> usize {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum ObjectRef {
    Null,
    Import(ImportIndex),
    Export(ExportIndex),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CompactNameRef {
    pub index: NameIndex,
    pub wire: CompactInt,
}

impl CompactNameRef {
    pub fn checked(wire: CompactInt, names: usize, field: &str) -> Result<Self, String> {
        Ok(Self {
            index: NameIndex::checked(wire.get(), names, field)?,
            wire,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CompactObjectRef {
    pub reference: ObjectRef,
    pub wire: CompactInt,
}

impl CompactObjectRef {
    pub fn checked(
        wire: CompactInt,
        imports: usize,
        exports: usize,
        field: &str,
    ) -> Result<Self, String> {
        Ok(Self {
            reference: ObjectRef::checked(wire.get(), imports, exports, field)?,
            wire,
        })
    }
}

impl ObjectRef {
    pub fn checked(wire: i32, imports: usize, exports: usize, field: &str) -> Result<Self, String> {
        if wire == 0 {
            return Ok(Self::Null);
        }
        if wire > 0 {
            let index = wire as usize - 1;
            return (index < exports)
                .then_some(Self::Export(ExportIndex(index)))
                .ok_or_else(|| format!("invalid {field} export reference {wire}"));
        }
        let index = wire.unsigned_abs() as usize - 1;
        (index < imports)
            .then_some(Self::Import(ImportIndex(index)))
            .ok_or_else(|| format!("invalid {field} import reference {wire}"))
    }

    pub fn wire(self) -> i32 {
        match self {
            Self::Null => 0,
            Self::Import(index) => -(index.get() as i32 + 1),
            Self::Export(index) => index.get() as i32 + 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn object_references_validate_and_round_trip_wire_values() {
        for wire in [-2, -1, 0, 1, 2] {
            assert_eq!(ObjectRef::checked(wire, 2, 2, "test").unwrap().wire(), wire);
        }
        assert!(ObjectRef::checked(3, 2, 2, "test").is_err());
        assert!(ObjectRef::checked(-3, 2, 2, "test").is_err());
    }

    #[test]
    fn package_version_predicates_cover_thresholds() {
        assert!(!PackageVersion::new(116, 0).has_terrain_vertex_colors());
        assert!(PackageVersion::new(117, 0).has_terrain_vertex_colors());
        assert!(!PackageVersion::new(123, 3).has_environment_lights());
        assert!(PackageVersion::new(123, 4).has_environment_lights());
        assert!(!PackageVersion::new(123, 11).has_static_mesh_unknown_indices());
        assert!(PackageVersion::new(123, 12).has_static_mesh_unknown_indices());
    }
}
