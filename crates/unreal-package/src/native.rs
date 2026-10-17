use crate::codec::BinaryReader;
use crate::model::{ObjectRef, PackageVersion};
use serde::{Deserialize, Serialize};

mod float_serde {
    use std::fmt;

    use serde::de::{Error, Visitor};
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    struct Float(f32);

    impl Serialize for Float {
        fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
        where
            S: Serializer,
        {
            if self.0.is_finite() {
                serializer.serialize_f32(self.0)
            } else {
                serializer.serialize_str(&format!("bits:{:08x}", self.0.to_bits()))
            }
        }
    }

    struct FloatVisitor;

    impl<'de> Visitor<'de> for FloatVisitor {
        type Value = Float;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a number or an IEEE-754 bits:XXXXXXXX string")
        }

        fn visit_f64<E: Error>(self, value: f64) -> Result<Self::Value, E> {
            Ok(Float(value as f32))
        }

        fn visit_i64<E: Error>(self, value: i64) -> Result<Self::Value, E> {
            Ok(Float(value as f32))
        }

        fn visit_u64<E: Error>(self, value: u64) -> Result<Self::Value, E> {
            Ok(Float(value as f32))
        }

        fn visit_str<E: Error>(self, value: &str) -> Result<Self::Value, E> {
            let bits = value
                .strip_prefix("bits:")
                .ok_or_else(|| E::custom("float string must start with bits:"))?;
            let bits = u32::from_str_radix(bits, 16).map_err(E::custom)?;
            Ok(Float(f32::from_bits(bits)))
        }

        fn visit_unit<E: Error>(self) -> Result<Self::Value, E> {
            Ok(Float(f32::NAN))
        }

        fn visit_none<E: Error>(self) -> Result<Self::Value, E> {
            self.visit_unit()
        }
    }

    impl<'de> Deserialize<'de> for Float {
        fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
        where
            D: Deserializer<'de>,
        {
            deserializer.deserialize_any(FloatVisitor)
        }
    }

    pub fn serialize<S>(value: &f32, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        Float(*value).serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<f32, D::Error>
    where
        D: Deserializer<'de>,
    {
        Float::deserialize(deserializer).map(|value| value.0)
    }

    pub mod option {
        use super::*;

        pub fn serialize<S>(value: &Option<f32>, serializer: S) -> Result<S::Ok, S::Error>
        where
            S: Serializer,
        {
            value.map(Float).serialize(serializer)
        }

        pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<f32>, D::Error>
        where
            D: Deserializer<'de>,
        {
            Option::<Float>::deserialize(deserializer).map(|value| value.map(|value| value.0))
        }
    }

    pub mod vec {
        use super::*;

        pub fn serialize<S>(value: &[f32], serializer: S) -> Result<S::Ok, S::Error>
        where
            S: Serializer,
        {
            value
                .iter()
                .copied()
                .map(Float)
                .collect::<Vec<_>>()
                .serialize(serializer)
        }

        pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<f32>, D::Error>
        where
            D: Deserializer<'de>,
        {
            Vec::<Float>::deserialize(deserializer)
                .map(|values| values.into_iter().map(|value| value.0).collect())
        }
    }

    pub mod array4 {
        use super::*;

        pub fn serialize<S>(value: &[f32; 4], serializer: S) -> Result<S::Ok, S::Error>
        where
            S: Serializer,
        {
            value.map(Float).serialize(serializer)
        }

        pub fn deserialize<'de, D>(deserializer: D) -> Result<[f32; 4], D::Error>
        where
            D: Deserializer<'de>,
        {
            <[Float; 4]>::deserialize(deserializer).map(|values| values.map(|value| value.0))
        }
    }
}

#[cfg(test)]
mod float_serde_tests {
    use super::Vector3;

    #[test]
    fn json_preserves_non_finite_float_bits() {
        let value = Vector3 {
            x: f32::from_bits(0x7fc1_2345),
            y: f32::INFINITY,
            z: -0.0,
        };
        let json = serde_json::to_string(&value).unwrap();
        assert!(json.contains("bits:7fc12345"));
        assert!(json.contains("bits:7f800000"));
        let decoded: Vector3 = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.x.to_bits(), value.x.to_bits());
        assert_eq!(decoded.y.to_bits(), value.y.to_bits());
        assert_eq!(decoded.z.to_bits(), value.z.to_bits());
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "camelCase")]
pub enum NativeObject {
    Level(Level),
    Model(Model),
    Polys(Polys),
    MeshAnimation(MeshAnimation),
    SkeletalMesh(Box<SkeletalMesh>),
    StaticMesh(StaticMesh),
    StaticMeshInstance(StaticMeshInstance),
    Shader(Shader),
    FinalBlend(FinalBlend),
    TexEnvMap(TexEnvMap),
    Texture(Texture),
    TerrainInfo(TerrainInfo),
    TerrainSector(TerrainSector),
    Emitter(EmitterNative),
    SpriteEmitter(EmitterNative),
    MeshEmitter(EmitterNative),
    VertMeshEmitter(EmitterNative),
    BeamEmitter(EmitterNative),
    Unknown,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Vector3 {
    #[serde(with = "float_serde")]
    pub x: f32,
    #[serde(with = "float_serde")]
    pub y: f32,
    #[serde(with = "float_serde")]
    pub z: f32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Quaternion {
    #[serde(with = "float_serde")]
    pub x: f32,
    #[serde(with = "float_serde")]
    pub y: f32,
    #[serde(with = "float_serde")]
    pub z: f32,
    #[serde(with = "float_serde")]
    pub w: f32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Rotator {
    pub pitch: i32,
    pub yaw: i32,
    pub roll: i32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoxBounds {
    pub min: Vector3,
    pub max: Vector3,
    pub valid: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SphereBounds {
    pub center: Vector3,
    #[serde(with = "float_serde")]
    pub radius: f32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkeletalMesh {
    pub bounding_box: BoxBounds,
    pub bounding_sphere: SphereBounds,
    pub mesh_version: i32,
    pub vertex_count: i32,
    pub source_vertices: Vec<Vector3>,
    pub materials: Vec<ObjectLink>,
    pub mesh_scale: Vector3,
    pub mesh_origin: Vector3,
    pub rotation_origin: Rotator,
    pub face_levels: Vec<u16>,
    pub inherited_faces: Vec<MeshFace>,
    pub inherited_collapse_wedges: Vec<u16>,
    pub inherited_wedges: Vec<MeshWedge>,
    pub material_slots: Vec<MeshMaterial>,
    #[serde(with = "float_serde")]
    pub mesh_scale_max: f32,
    #[serde(with = "float_serde")]
    pub lod_hysteresis: f32,
    #[serde(with = "float_serde")]
    pub lod_strength: f32,
    pub lod_min_vertices: i32,
    #[serde(with = "float_serde")]
    pub lod_morph: f32,
    #[serde(with = "float_serde")]
    pub lod_z_displacement: f32,
    pub impostor: Option<MeshImpostor>,
    #[serde(with = "float_serde::option")]
    pub skin_tessellation_factor: Option<f32>,
    pub lineage_lod_unknown: Option<i32>,
    pub lineage_version_6_byte: Option<u8>,
    pub lineage_version_7_byte: Option<u8>,
    pub secondary_points: Vec<Vector3>,
    pub skeleton: Vec<SkeletalBone>,
    pub animation: ObjectLink,
    pub skeletal_depth: i32,
    pub weight_indices: Vec<WeightIndex>,
    pub bone_influences: Vec<BoneInfluence>,
    pub sockets: Vec<MeshSocket>,
    pub lods: Vec<SkeletalLod>,
    pub legacy_object: ObjectLink,
    pub points: LazyArray<Vector3>,
    pub wedges: LazyArray<MeshWedge>,
    pub triangles: LazyArray<MeshTriangle>,
    pub influences: LazyArray<VertexInfluence>,
    pub collapse_wedges: LazyArray<u16>,
    pub triangle_points: LazyArray<u16>,
    pub lineage_unknown_1: Option<i32>,
    #[serde(with = "float_serde::vec")]
    pub lineage_floats: Vec<f32>,
    pub lineage_auth_key: Option<i32>,
    pub lineage_unknown_4: Option<i32>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshImpostor {
    pub enabled: i32,
    pub material: ObjectLink,
    pub location: Vector3,
    pub rotation: Rotator,
    pub scale: Vector3,
    pub color: u32,
    pub space_mode: i32,
    pub draw_mode: i32,
    pub light_mode: i32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WeightIndex {
    pub bone_influence_indices: Vec<u16>,
    pub start_bone_influence: i32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoneInfluence {
    pub bone_weight: u16,
    pub bone_index: u16,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LazyArray<T> {
    pub serialized_end_offset: i32,
    pub values: Vec<T>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkeletalLod {
    pub skinning_data: Vec<u32>,
    pub skin_points: Vec<SkinPoint>,
    pub soft_wedge_count: i32,
    pub soft_sections: Vec<SkeletalSection>,
    pub rigid_sections: Vec<SkeletalSection>,
    pub soft_indices: IndexBuffer,
    pub rigid_indices: IndexBuffer,
    pub vertex_stream_revision: i32,
    pub vertex_stream_unknown_1: i32,
    pub vertex_stream_unknown_2: i32,
    pub rigid_vertices: Vec<MeshVertex>,
    pub influences: LazyArray<VertexInfluence>,
    pub wedges: LazyArray<MeshWedge>,
    pub faces: LazyArray<MeshFace>,
    pub points: LazyArray<Vector3>,
    #[serde(with = "float_serde")]
    pub distance_factor: f32,
    #[serde(with = "float_serde")]
    pub hysteresis: f32,
    pub shared_vertex_count: i32,
    pub max_influences: i32,
    pub unknown_114: i32,
    pub unknown_118: i32,
    pub use_new_wedges: bool,
    pub lineage_vertices: Vec<LineageVertex>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkinPoint {
    pub position: Vector3,
    pub packed_normal: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkeletalSection {
    pub material_index: u16,
    pub min_stream_index: u16,
    pub min_wedge_index: u16,
    pub max_wedge_index: u16,
    pub stream_index_count: u16,
    pub bone_index: u16,
    pub unknown_e: u16,
    pub first_face: u16,
    pub face_count: u16,
    pub bone_map: Vec<i32>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexBuffer {
    pub indices: Vec<u16>,
    pub revision: i32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshVertex {
    pub position: Vector3,
    pub normal: Vector3,
    #[serde(with = "float_serde")]
    pub u: f32,
    #[serde(with = "float_serde")]
    pub v: f32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LineageVertex {
    pub position: Vector3,
    pub normal: Vector3,
    #[serde(with = "float_serde")]
    pub u: f32,
    #[serde(with = "float_serde")]
    pub v: f32,
    pub bones: [u8; 4],
    #[serde(with = "float_serde::array4")]
    pub weights: [f32; 4],
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VertexInfluence {
    #[serde(with = "float_serde")]
    pub weight: f32,
    pub point_index: u16,
    pub bone_index: u16,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshWedge {
    pub vertex_index: u16,
    #[serde(with = "float_serde")]
    pub u: f32,
    #[serde(with = "float_serde")]
    pub v: f32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshFace {
    pub wedge_indices: [u16; 3],
    pub material_index: u16,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshTriangle {
    pub wedge_indices: [u16; 3],
    pub material_index: u8,
    pub auxiliary_material_index: u8,
    pub smoothing_groups: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshMaterial {
    pub polygon_flags: u32,
    pub texture_index: i32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkeletalBone {
    pub name: String,
    pub flags: u32,
    pub orientation: Quaternion,
    pub position: Vector3,
    #[serde(with = "float_serde")]
    pub length: f32,
    pub size: Vector3,
    pub child_count: i32,
    pub parent_index: i32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Coordinates {
    pub origin: Vector3,
    pub x_axis: Vector3,
    pub y_axis: Vector3,
    pub z_axis: Vector3,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshSocket {
    pub alias: String,
    pub bone: String,
    pub coordinates: Coordinates,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Level {
    pub actors: Vec<ObjectLink>,
    pub trailing_data: Vec<u8>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Model {
    pub bounding_box: BoxBounds,
    pub bounding_sphere: SphereBounds,
    pub serialized_bsp: Vec<u8>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Polys {
    pub database_count: i32,
    pub database_capacity: i32,
    pub serialized_polygons: Vec<u8>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StaticMesh {
    pub primitive_bounding_box: BoxBounds,
    pub bounding_sphere: SphereBounds,
    pub sections: Vec<StaticMeshSection>,
    pub bounding_box: BoxBounds,
    pub vertex_stream: StaticMeshVertexStream,
    pub color_stream: ColorStream,
    pub alpha_stream: ColorStream,
    pub uv_streams: Vec<StaticMeshUvStream>,
    pub primary_indices: IndexBuffer,
    pub secondary_indices: IndexBuffer,
    pub collision_object: ObjectLink,
    pub collision_data: Vec<u8>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StaticMeshSection {
    pub unknown_4: i32,
    pub first_index: u16,
    pub first_vertex: u16,
    pub last_vertex: u16,
    pub unknown_e: u16,
    pub face_count: u16,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StaticMeshVertexStream {
    pub vertices: Vec<StaticMeshVertex>,
    pub revision: i32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StaticMeshVertex {
    pub position: Vector3,
    pub normal: Vector3,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ColorStream {
    pub colors: Vec<Color>,
    pub revision: i32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StaticMeshUvStream {
    pub coordinates: Vec<TextureCoordinate>,
    pub unknown_10: i32,
    pub unknown_1c: i32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextureCoordinate {
    #[serde(with = "float_serde")]
    pub u: f32,
    #[serde(with = "float_serde")]
    pub v: f32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshAnimation {
    pub version: i32,
    pub bones: Vec<AnimationBone>,
    pub moves_end_offset: i32,
    pub moves: Vec<AnimationMove>,
    pub sequences: Vec<AnimationSequence>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnimationMove {
    pub serialized_end_offset: i32,
    pub root_speed: Vector3,
    #[serde(with = "float_serde")]
    pub track_time: f32,
    pub start_bone: i32,
    pub flags: u32,
    pub bone_indices: Vec<i32>,
    pub tracks: Vec<AnalogTrack>,
    pub root_track: AnalogTrack,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalogTrack {
    pub flags: u32,
    pub orientations: Vec<Quaternion>,
    pub positions: Vec<Vector3>,
    #[serde(with = "float_serde::vec")]
    pub times: Vec<f32>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnimationBone {
    pub name: String,
    pub flags: u32,
    pub parent_index: i32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnimationSequence {
    #[serde(with = "float_serde")]
    pub unknown_28: f32,
    pub name: String,
    pub groups: Vec<String>,
    pub start_frame: i32,
    pub frame_count: i32,
    #[serde(with = "float_serde")]
    pub rate: f32,
    pub notifications: Vec<AnimationNotification>,
    pub lineage: LineageSequenceData,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnimationNotification {
    #[serde(with = "float_serde")]
    pub notify_frame: f32,
    pub function: String,
    pub notify: ObjectLink,
    pub lineage_text: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LineageSequenceData {
    pub unknown_2c: i32,
    pub unknown_30: i32,
    pub unknown_34: Option<i32>,
    pub object: ObjectLink,
    pub unknown_3c: Option<i32>,
    pub unknown_40: Option<i32>,
    pub extension: Option<LineageSequenceExtension>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LineagePair {
    pub first: i32,
    pub second: i32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LineageNestedPairs {
    pub value: i32,
    pub pairs: Vec<LineagePair>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LineageSequenceExtension {
    pub flags: Option<u8>,
    pub pairs: Vec<LineagePair>,
    pub nested_pairs: Vec<LineageNestedPairs>,
    pub unknown_1c: Option<i32>,
    pub unknown_20: Option<i32>,
    pub trailing_pairs: Vec<LineagePair>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Color {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
    pub alpha: u8,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Texture {
    pub material: Option<LineageMaterial>,
    pub mipmaps: Vec<TextureMip>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TexEnvMap {
    pub material: Option<LineageMaterial>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Shader {
    pub material: Option<LineageMaterial>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FinalBlend {
    pub material: Option<LineageMaterial>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmitterNative {}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LineageMaterial {
    pub shader_property: LineageShaderProperty,
    pub shader_code: String,
    pub material_code_version: [u16; 2],
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LineageShaderProperty {
    pub flags: [u8; 2],
    pub passes: Vec<LineageShaderPass>,
    pub fade_colors: [u8; 8],
    pub fade_period: i32,
    pub fade_phase: i32,
    pub color_fade_type: i32,
    pub stages: Vec<LineageMaterialStage>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LineageShaderPass {
    pub flags: Vec<u8>,
    pub legacy_value: Option<i32>,
    pub blend_mode: i32,
    pub fog_color: i32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LineageMaterialStage {
    pub name: String,
    pub textures: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextureMip {
    pub serialized_end_offset: u32,
    pub data: Vec<u8>,
    pub width: i32,
    pub height: i32,
    pub width_bits: u8,
    pub height_bits: u8,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerrainInfo {
    pub sectors: Vec<i32>,
    pub vertices: Vec<Vector3>,
    pub sectors_x: i32,
    pub sectors_y: i32,
    pub face_normals: Vec<TerrainNormalPair>,
    pub to_world: Coordinates,
    pub to_height_map: Coordinates,
    pub heightmap_x: i32,
    pub heightmap_y: i32,
    pub vertex_colors: Vec<Color>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerrainNormalPair {
    pub first: Vector3,
    pub second: Vector3,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerrainSector {
    pub info: ObjectLink,
    pub quads_x: i32,
    pub quads_y: i32,
    pub offset_x: i32,
    pub offset_y: i32,
    pub bounding_box: BoxBounds,
    pub light_infos: Vec<TerrainLightInfo>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerrainLightInfo {
    pub light: ObjectLink,
    pub data: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ObjectLink {
    pub reference: ObjectReference,
    pub wire_value: i32,
    pub path: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ObjectReference {
    pub kind: ObjectReferenceKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index: Option<usize>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ObjectReferenceKind {
    Null,
    Import,
    Export,
}

impl From<ObjectRef> for ObjectReference {
    fn from(value: ObjectRef) -> Self {
        match value {
            ObjectRef::Null => Self {
                kind: ObjectReferenceKind::Null,
                index: None,
            },
            ObjectRef::Import(index) => Self {
                kind: ObjectReferenceKind::Import,
                index: Some(index.get()),
            },
            ObjectRef::Export(index) => Self {
                kind: ObjectReferenceKind::Export,
                index: Some(index.get()),
            },
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StaticMeshLightInfo {
    pub light: ObjectLink,
    pub vertex_flags: Vec<u8>,
    pub applied: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StaticMeshInstance {
    pub color_stream: Vec<Color>,
    pub color_revision: i32,
    pub scene_lights: Vec<StaticMeshLightInfo>,
    pub environment_lights: Vec<StaticMeshLightInfo>,
    pub unknown_indices: Vec<i32>,
    #[serde(skip)]
    pub original: Vec<u8>,
}

pub struct NativeDecode {
    pub object: NativeObject,
    pub consumed: usize,
}

pub struct NativeContext<'a> {
    pub version: PackageVersion,
    pub imports: usize,
    pub exports: usize,
    pub object_offset: usize,
    pub resolve_name: &'a dyn Fn(i32) -> String,
    pub resolve: &'a dyn Fn(ObjectRef) -> String,
}

pub struct NativeObjectRegistry;

impl NativeObjectRegistry {
    pub fn decode(
        class: &str,
        data: &[u8],
        context: &NativeContext<'_>,
    ) -> Result<NativeDecode, String> {
        let mut reader = BinaryReader::new(data);
        let object = match class {
            "Engine.Level" => NativeObject::Level(level(&mut reader, context)?),
            "Engine.Model" => NativeObject::Model(model(&mut reader)?),
            "Engine.Polys" => NativeObject::Polys(polys(&mut reader)?),
            "Engine.MeshAnimation" => {
                NativeObject::MeshAnimation(mesh_animation(&mut reader, context)?)
            }
            "Engine.SkeletalMesh" => {
                NativeObject::SkeletalMesh(Box::new(skeletal_mesh(&mut reader, context)?))
            }
            "Engine.StaticMesh" => NativeObject::StaticMesh(static_mesh(&mut reader, context)?),
            "Engine.StaticMeshInstance" => {
                NativeObject::StaticMeshInstance(static_mesh_instance(&mut reader, data, context)?)
            }
            "Engine.Shader" => NativeObject::Shader(Shader {
                material: optional_lineage_material(&mut reader, context.version)?,
            }),
            "Engine.FinalBlend" => NativeObject::FinalBlend(FinalBlend {
                material: optional_lineage_material(&mut reader, context.version)?,
            }),
            "Engine.TexEnvMap" => NativeObject::TexEnvMap(TexEnvMap {
                material: optional_lineage_material(&mut reader, context.version)?,
            }),
            "Engine.Texture" => NativeObject::Texture(texture(&mut reader, context.version)?),
            "Engine.TerrainInfo" => {
                NativeObject::TerrainInfo(terrain_info(&mut reader, context.version)?)
            }
            "Engine.TerrainSector" => {
                NativeObject::TerrainSector(terrain_sector(&mut reader, context)?)
            }
            "Engine.Emitter" => NativeObject::Emitter(EmitterNative::default()),
            "Engine.SpriteEmitter" => NativeObject::SpriteEmitter(EmitterNative::default()),
            "Engine.MeshEmitter" => NativeObject::MeshEmitter(EmitterNative::default()),
            "Engine.VertMeshEmitter" => NativeObject::VertMeshEmitter(EmitterNative::default()),
            "Engine.BeamEmitter" => NativeObject::BeamEmitter(EmitterNative::default()),
            _ => NativeObject::Unknown,
        };
        Ok(NativeDecode {
            consumed: reader.position(),
            object,
        })
    }
}

fn model(reader: &mut BinaryReader<'_>) -> Result<Model, String> {
    Ok(Model {
        bounding_box: box_bounds(reader)?,
        bounding_sphere: SphereBounds {
            center: vector(reader)?,
            radius: reader.f32().map_err(codec)?,
        },
        serialized_bsp: remaining_bytes(reader)?,
    })
}

fn polys(reader: &mut BinaryReader<'_>) -> Result<Polys, String> {
    let database_count = reader.i32().map_err(codec)?;
    let database_capacity = reader.i32().map_err(codec)?;
    if database_count < 0 || database_capacity < database_count {
        return Err(format!(
            "invalid polygon database size {database_count}/{database_capacity}"
        ));
    }
    Ok(Polys {
        database_count,
        database_capacity,
        serialized_polygons: remaining_bytes(reader)?,
    })
}

fn level(reader: &mut BinaryReader<'_>, context: &NativeContext<'_>) -> Result<Level, String> {
    let capacity = usize::try_from(reader.i32().map_err(codec)?)
        .map_err(|_| "negative level actor capacity")?;
    let count =
        usize::try_from(reader.i32().map_err(codec)?).map_err(|_| "negative level actor count")?;
    if count > capacity {
        return Err(format!(
            "level actor count {count} exceeds capacity {capacity}"
        ));
    }
    let mut actors = Vec::with_capacity(count);
    for _ in 0..count {
        let wire_value = reader.compact_int().map_err(codec)?.get();
        let reference =
            ObjectRef::checked(wire_value, context.imports, context.exports, "level actor")?;
        actors.push(ObjectLink {
            reference: reference.into(),
            wire_value,
            path: (context.resolve)(reference),
        });
    }
    Ok(Level {
        actors,
        trailing_data: remaining_bytes(reader)?,
    })
}

fn static_mesh(
    reader: &mut BinaryReader<'_>,
    context: &NativeContext<'_>,
) -> Result<StaticMesh, String> {
    let primitive_bounding_box = box_bounds(reader)?;
    let bounding_sphere = SphereBounds {
        center: vector(reader)?,
        radius: reader.f32().map_err(codec)?,
    };
    let section_count = reader
        .compact_count()
        .map_err(codec)
        .map_err(|error| format!("static mesh sections at {}: {error}", reader.position()))?;
    let mut sections = Vec::with_capacity(section_count);
    for _ in 0..section_count {
        sections.push(StaticMeshSection {
            unknown_4: reader.i32().map_err(codec)?,
            first_index: reader.u16().map_err(codec)?,
            first_vertex: reader.u16().map_err(codec)?,
            last_vertex: reader.u16().map_err(codec)?,
            unknown_e: reader.u16().map_err(codec)?,
            face_count: reader.u16().map_err(codec)?,
        });
    }
    let bounding_box = box_bounds(reader)?;
    let vertex_count = reader
        .compact_count()
        .map_err(codec)
        .map_err(|error| format!("static mesh vertices at {}: {error}", reader.position()))?;
    let mut vertices = Vec::with_capacity(vertex_count);
    for _ in 0..vertex_count {
        vertices.push(StaticMeshVertex {
            position: vector(reader)?,
            normal: vector(reader)?,
        });
    }
    let vertex_stream = StaticMeshVertexStream {
        vertices,
        revision: reader.i32().map_err(codec)?,
    };
    let color_stream = decode_color_stream(reader)
        .map_err(|error| format!("static mesh colors at {}: {error}", reader.position()))?;
    let alpha_stream = decode_color_stream(reader)
        .map_err(|error| format!("static mesh alpha at {}: {error}", reader.position()))?;
    let uv_count = reader
        .compact_count()
        .map_err(codec)
        .map_err(|error| format!("static mesh UV streams at {}: {error}", reader.position()))?;
    let mut uv_streams = Vec::with_capacity(uv_count);
    for _ in 0..uv_count {
        let coordinate_count = reader.compact_count().map_err(codec)?;
        let mut coordinates = Vec::with_capacity(coordinate_count);
        for _ in 0..coordinate_count {
            coordinates.push(TextureCoordinate {
                u: reader.f32().map_err(codec)?,
                v: reader.f32().map_err(codec)?,
            });
        }
        uv_streams.push(StaticMeshUvStream {
            coordinates,
            unknown_10: reader.i32().map_err(codec)?,
            unknown_1c: reader.i32().map_err(codec)?,
        });
    }
    Ok(StaticMesh {
        primitive_bounding_box,
        bounding_sphere,
        sections,
        bounding_box,
        vertex_stream,
        color_stream,
        alpha_stream,
        uv_streams,
        primary_indices: index_buffer(reader)
            .map_err(|error| format!("static mesh primary indices: {error}"))?,
        secondary_indices: index_buffer(reader)
            .map_err(|error| format!("static mesh secondary indices: {error}"))?,
        collision_object: compact_object_link(reader, context, "static mesh collision object")?,
        collision_data: remaining_bytes(reader)?,
    })
}

fn box_bounds(reader: &mut BinaryReader<'_>) -> Result<BoxBounds, String> {
    Ok(BoxBounds {
        min: vector(reader)?,
        max: vector(reader)?,
        valid: reader.u8().map_err(codec)? != 0,
    })
}

fn decode_color_stream(reader: &mut BinaryReader<'_>) -> Result<ColorStream, String> {
    Ok(ColorStream {
        colors: color_array(reader)?,
        revision: reader.i32().map_err(codec)?,
    })
}

fn skeletal_mesh(
    reader: &mut BinaryReader<'_>,
    context: &NativeContext<'_>,
) -> Result<SkeletalMesh, String> {
    let bounding_box = BoxBounds {
        min: vector(reader)?,
        max: vector(reader)?,
        valid: reader.u8().map_err(codec)? != 0,
    };
    let bounding_sphere = SphereBounds {
        center: vector(reader)?,
        radius: reader.f32().map_err(codec)?,
    };
    let mesh_version = reader.i32().map_err(codec)?;
    let vertex_count = reader.i32().map_err(codec)?;
    let source_vertex_count = reader.compact_count().map_err(codec)?;
    let mut source_vertices = Vec::with_capacity(source_vertex_count);
    for _ in 0..source_vertex_count {
        source_vertices.push(vector(reader)?);
    }
    let materials = object_links(reader, context, "skeletal mesh material")?;
    let mesh_scale = vector(reader)?;
    let mesh_origin = vector(reader)?;
    let rotation_origin = rotator(reader)?;

    let face_levels = u16_array(reader)?;
    let inherited_faces = mesh_face_array(reader)?;
    let inherited_collapse_wedges = u16_array(reader)?;
    let inherited_wedges = mesh_wedge_array(reader)?;
    let material_count = reader.compact_count().map_err(codec)?;
    let mut material_slots = Vec::with_capacity(material_count);
    for _ in 0..material_count {
        material_slots.push(MeshMaterial {
            polygon_flags: reader.u32().map_err(codec)?,
            texture_index: reader.i32().map_err(codec)?,
        });
    }
    let mesh_scale_max = reader.f32().map_err(codec)?;
    let lod_hysteresis = reader.f32().map_err(codec)?;
    let lod_strength = reader.f32().map_err(codec)?;
    let lod_min_vertices = reader.i32().map_err(codec)?;
    let lod_morph = reader.f32().map_err(codec)?;
    let lod_z_displacement = reader.f32().map_err(codec)?;

    let impostor = if mesh_version >= 3 {
        Some(MeshImpostor {
            enabled: reader.i32().map_err(codec)?,
            material: compact_object_link(reader, context, "impostor material")?,
            location: vector(reader)?,
            rotation: rotator(reader)?,
            scale: vector(reader)?,
            color: reader.u32().map_err(codec)?,
            space_mode: reader.i32().map_err(codec)?,
            draw_mode: reader.i32().map_err(codec)?,
            light_mode: reader.i32().map_err(codec)?,
        })
    } else {
        None
    };
    let skin_tessellation_factor = (mesh_version >= 4)
        .then(|| reader.f32().map_err(codec))
        .transpose()?;
    let lineage_lod_unknown = (mesh_version >= 5)
        .then(|| reader.i32().map_err(codec))
        .transpose()?;
    let lineage_version_6_byte = (mesh_version >= 6)
        .then(|| reader.u8().map_err(codec))
        .transpose()?;
    // Lineage version 7 packages contain one additional byte not documented by UEViewer.
    let lineage_version_7_byte = (mesh_version >= 7)
        .then(|| reader.u8().map_err(codec))
        .transpose()?;

    let secondary_points = vector_array(reader)?;
    let bone_count = reader.compact_count().map_err(codec)?;
    let mut skeleton = Vec::with_capacity(bone_count);
    for _ in 0..bone_count {
        skeleton.push(SkeletalBone {
            name: compact_name(reader, context)?,
            flags: reader.u32().map_err(codec)?,
            orientation: quaternion(reader)?,
            position: vector(reader)?,
            length: reader.f32().map_err(codec)?,
            size: vector(reader)?,
            child_count: reader.i32().map_err(codec)?,
            parent_index: reader.i32().map_err(codec)?,
        });
    }
    for (index, bone) in skeleton.iter().enumerate() {
        if bone.parent_index < 0 || bone.parent_index as usize >= bone_count {
            return Err(format!(
                "skeletal bone {index} has invalid parent {}",
                bone.parent_index
            ));
        }
    }
    let animation = compact_object_link(reader, context, "skeletal mesh animation")?;
    let skeletal_depth = reader.i32().map_err(codec)?;
    let weight_index_count = reader.compact_count().map_err(codec)?;
    let mut weight_indices = Vec::with_capacity(weight_index_count);
    for _ in 0..weight_index_count {
        weight_indices.push(WeightIndex {
            bone_influence_indices: u16_array(reader)?,
            start_bone_influence: reader.i32().map_err(codec)?,
        });
    }
    let bone_influence_count = reader.compact_count().map_err(codec)?;
    let mut bone_influences = Vec::with_capacity(bone_influence_count);
    for _ in 0..bone_influence_count {
        bone_influences.push(BoneInfluence {
            bone_weight: reader.u16().map_err(codec)?,
            bone_index: reader.u16().map_err(codec)?,
        });
    }

    let aliases = name_array(reader, context)?;
    let bones = name_array(reader, context)?;
    let coordinate_count = reader.compact_count().map_err(codec)?;
    let mut coordinates = Vec::with_capacity(coordinate_count);
    for _ in 0..coordinate_count {
        coordinates.push(coordinates_value(reader)?);
    }
    if aliases.len() != bones.len() || aliases.len() != coordinates.len() {
        return Err(format!(
            "skeletal socket arrays differ in length: aliases={}, bones={}, coordinates={}",
            aliases.len(),
            bones.len(),
            coordinates.len()
        ));
    }
    let sockets = aliases
        .into_iter()
        .zip(bones)
        .zip(coordinates)
        .map(|((alias, bone), coordinates)| MeshSocket {
            alias,
            bone,
            coordinates,
        })
        .collect();
    let lod_count = reader.compact_count().map_err(codec)?;
    let mut lods = Vec::with_capacity(lod_count);
    for _ in 0..lod_count {
        lods.push(skeletal_lod(reader, context)?);
    }
    let legacy_object = compact_object_link(reader, context, "skeletal mesh legacy object")?;
    let points = lazy_vectors(reader)?;
    let wedges = lazy_wedges(reader)?;
    let triangles = lazy_triangles(reader)?;
    let influences = lazy_influences(reader)?;
    let collapse_wedges = lazy_u16(reader)?;
    let triangle_points = lazy_u16(reader)?;

    let lineage_unknown_1 = (context.version.archive >= 118 && context.version.license >= 3)
        .then(|| reader.i32().map_err(codec))
        .transpose()?;
    let lineage_floats = if context.version.archive >= 123 && context.version.license >= 0x12 {
        f32_array(reader)?
    } else {
        Vec::new()
    };
    let lineage_auth_key = (context.version.archive >= 120)
        .then(|| reader.i32().map_err(codec))
        .transpose()?;
    let lineage_unknown_4 = (context.version.license >= 0x23)
        .then(|| reader.i32().map_err(codec))
        .transpose()?;

    Ok(SkeletalMesh {
        bounding_box,
        bounding_sphere,
        mesh_version,
        vertex_count,
        source_vertices,
        materials,
        mesh_scale,
        mesh_origin,
        rotation_origin,
        face_levels,
        inherited_faces,
        inherited_collapse_wedges,
        inherited_wedges,
        material_slots,
        mesh_scale_max,
        lod_hysteresis,
        lod_strength,
        lod_min_vertices,
        lod_morph,
        lod_z_displacement,
        impostor,
        skin_tessellation_factor,
        lineage_lod_unknown,
        lineage_version_6_byte,
        lineage_version_7_byte,
        secondary_points,
        skeleton,
        animation,
        skeletal_depth,
        weight_indices,
        bone_influences,
        sockets,
        lods,
        legacy_object,
        points,
        wedges,
        triangles,
        influences,
        collapse_wedges,
        triangle_points,
        lineage_unknown_1,
        lineage_floats,
        lineage_auth_key,
        lineage_unknown_4,
    })
}

fn skeletal_lod(
    reader: &mut BinaryReader<'_>,
    context: &NativeContext<'_>,
) -> Result<SkeletalLod, String> {
    let skinning_data = u32_array(reader)?;
    let skin_point_count = reader.compact_count().map_err(codec)?;
    let mut skin_points = Vec::with_capacity(skin_point_count);
    for _ in 0..skin_point_count {
        skin_points.push(SkinPoint {
            position: vector(reader)?,
            packed_normal: reader.u32().map_err(codec)?,
        });
    }
    let soft_wedge_count = reader.i32().map_err(codec)?;
    let soft_sections = skeletal_sections(reader, context.version)?;
    let rigid_sections = skeletal_sections(reader, context.version)?;
    let soft_indices = index_buffer(reader)?;
    let rigid_indices = index_buffer(reader)?;
    let vertex_stream_revision = reader.i32().map_err(codec)?;
    let vertex_stream_unknown_1 = reader.i32().map_err(codec)?;
    let vertex_stream_unknown_2 = reader.i32().map_err(codec)?;
    let rigid_vertex_count = reader.compact_count().map_err(codec)?;
    let mut rigid_vertices = Vec::with_capacity(rigid_vertex_count);
    for _ in 0..rigid_vertex_count {
        rigid_vertices.push(mesh_vertex(reader)?);
    }
    let influences = lazy_influences(reader)?;
    let wedges = lazy_wedges(reader)?;
    let faces = lazy_faces(reader)?;
    let points = lazy_vectors(reader)?;
    let distance_factor = reader.f32().map_err(codec)?;
    let hysteresis = reader.f32().map_err(codec)?;
    let shared_vertex_count = reader.i32().map_err(codec)?;
    let max_influences = reader.i32().map_err(codec)?;
    let unknown_114 = reader.i32().map_err(codec)?;
    let unknown_118 = reader.i32().map_err(codec)?;
    let use_new_wedges = reader.i32().map_err(codec)? != 0;
    let lineage_vertex_count = reader.compact_count().map_err(codec)?;
    let mut lineage_vertices = Vec::with_capacity(lineage_vertex_count);
    for _ in 0..lineage_vertex_count {
        lineage_vertices.push(lineage_vertex(reader)?);
    }
    Ok(SkeletalLod {
        skinning_data,
        skin_points,
        soft_wedge_count,
        soft_sections,
        rigid_sections,
        soft_indices,
        rigid_indices,
        vertex_stream_revision,
        vertex_stream_unknown_1,
        vertex_stream_unknown_2,
        rigid_vertices,
        influences,
        wedges,
        faces,
        points,
        distance_factor,
        hysteresis,
        shared_vertex_count,
        max_influences,
        unknown_114,
        unknown_118,
        use_new_wedges,
        lineage_vertices,
    })
}

fn skeletal_sections(
    reader: &mut BinaryReader<'_>,
    version: PackageVersion,
) -> Result<Vec<SkeletalSection>, String> {
    let count = reader.compact_count().map_err(codec)?;
    let mut sections = Vec::with_capacity(count);
    for _ in 0..count {
        let material_index = reader.u16().map_err(codec)?;
        let min_stream_index = reader.u16().map_err(codec)?;
        let min_wedge_index = reader.u16().map_err(codec)?;
        let max_wedge_index = reader.u16().map_err(codec)?;
        let stream_index_count = reader.u16().map_err(codec)?;
        let bone_index = reader.u16().map_err(codec)?;
        let unknown_e = reader.u16().map_err(codec)?;
        let first_face = reader.u16().map_err(codec)?;
        let face_count = reader.u16().map_err(codec)?;
        let bone_map = if version.license >= 0x1c {
            i32_array(reader)?
        } else {
            Vec::new()
        };
        sections.push(SkeletalSection {
            material_index,
            min_stream_index,
            min_wedge_index,
            max_wedge_index,
            stream_index_count,
            bone_index,
            unknown_e,
            first_face,
            face_count,
            bone_map,
        });
    }
    Ok(sections)
}

fn index_buffer(reader: &mut BinaryReader<'_>) -> Result<IndexBuffer, String> {
    Ok(IndexBuffer {
        indices: u16_array(reader)?,
        revision: reader.i32().map_err(codec)?,
    })
}

fn mesh_vertex(reader: &mut BinaryReader<'_>) -> Result<MeshVertex, String> {
    Ok(MeshVertex {
        position: vector(reader)?,
        normal: vector(reader)?,
        u: reader.f32().map_err(codec)?,
        v: reader.f32().map_err(codec)?,
    })
}

fn lineage_vertex(reader: &mut BinaryReader<'_>) -> Result<LineageVertex, String> {
    let position = vector(reader)?;
    let normal = vector(reader)?;
    let u = reader.f32().map_err(codec)?;
    let v = reader.f32().map_err(codec)?;
    let bones = [
        reader.u8().map_err(codec)?,
        reader.u8().map_err(codec)?,
        reader.u8().map_err(codec)?,
        reader.u8().map_err(codec)?,
    ];
    let weights = [
        reader.f32().map_err(codec)?,
        reader.f32().map_err(codec)?,
        reader.f32().map_err(codec)?,
        reader.f32().map_err(codec)?,
    ];
    Ok(LineageVertex {
        position,
        normal,
        u,
        v,
        bones,
        weights,
    })
}

fn vector(reader: &mut BinaryReader<'_>) -> Result<Vector3, String> {
    Ok(Vector3 {
        x: reader.f32().map_err(codec)?,
        y: reader.f32().map_err(codec)?,
        z: reader.f32().map_err(codec)?,
    })
}

fn quaternion(reader: &mut BinaryReader<'_>) -> Result<Quaternion, String> {
    Ok(Quaternion {
        x: reader.f32().map_err(codec)?,
        y: reader.f32().map_err(codec)?,
        z: reader.f32().map_err(codec)?,
        w: reader.f32().map_err(codec)?,
    })
}

fn rotator(reader: &mut BinaryReader<'_>) -> Result<Rotator, String> {
    Ok(Rotator {
        pitch: reader.i32().map_err(codec)?,
        yaw: reader.i32().map_err(codec)?,
        roll: reader.i32().map_err(codec)?,
    })
}

fn coordinates_value(reader: &mut BinaryReader<'_>) -> Result<Coordinates, String> {
    Ok(Coordinates {
        origin: vector(reader)?,
        x_axis: vector(reader)?,
        y_axis: vector(reader)?,
        z_axis: vector(reader)?,
    })
}

fn u32_array(reader: &mut BinaryReader<'_>) -> Result<Vec<u32>, String> {
    let count = reader.compact_count().map_err(codec)?;
    let mut values = Vec::with_capacity(count);
    for _ in 0..count {
        values.push(reader.u32().map_err(codec)?);
    }
    Ok(values)
}

fn f32_array(reader: &mut BinaryReader<'_>) -> Result<Vec<f32>, String> {
    let count = reader.compact_count().map_err(codec)?;
    let mut values = Vec::with_capacity(count);
    for _ in 0..count {
        values.push(reader.f32().map_err(codec)?);
    }
    Ok(values)
}

fn vector_array(reader: &mut BinaryReader<'_>) -> Result<Vec<Vector3>, String> {
    let count = reader.compact_count().map_err(codec)?;
    let mut values = Vec::with_capacity(count);
    for _ in 0..count {
        values.push(vector(reader)?);
    }
    Ok(values)
}

fn quaternion_array(reader: &mut BinaryReader<'_>) -> Result<Vec<Quaternion>, String> {
    let count = reader.compact_count().map_err(codec)?;
    let mut values = Vec::with_capacity(count);
    for _ in 0..count {
        values.push(quaternion(reader)?);
    }
    Ok(values)
}

fn color_array(reader: &mut BinaryReader<'_>) -> Result<Vec<Color>, String> {
    let count = reader.compact_count().map_err(codec)?;
    let mut values = Vec::with_capacity(count);
    for _ in 0..count {
        values.push(Color {
            red: reader.u8().map_err(codec)?,
            green: reader.u8().map_err(codec)?,
            blue: reader.u8().map_err(codec)?,
            alpha: reader.u8().map_err(codec)?,
        });
    }
    Ok(values)
}

fn utf16_string(reader: &mut BinaryReader<'_>) -> Result<String, String> {
    let length = usize::try_from(reader.i32().map_err(codec)?)
        .map_err(|_| "negative UTF-16 string length")?;
    let mut units = Vec::with_capacity(length);
    for _ in 0..length {
        units.push(reader.u16().map_err(codec)?);
    }
    if units.last() == Some(&0) {
        units.pop();
    }
    Ok(String::from_utf16_lossy(&units))
}

fn unreal_string(reader: &mut BinaryReader<'_>) -> Result<String, String> {
    let length = reader.compact_int().map_err(codec)?.get();
    if length == 0 {
        return Ok(String::new());
    }
    if length > 0 {
        let bytes = reader.take(length as usize).map_err(codec)?;
        return Ok(String::from_utf8_lossy(bytes.strip_suffix(&[0]).unwrap_or(bytes)).into_owned());
    }
    let unit_count = usize::try_from(-i64::from(length)).map_err(|_| "string length overflow")?;
    let mut units = Vec::with_capacity(unit_count);
    for _ in 0..unit_count {
        units.push(reader.u16().map_err(codec)?);
    }
    if units.last() == Some(&0) {
        units.pop();
    }
    Ok(String::from_utf16_lossy(&units))
}

fn mesh_face_array(reader: &mut BinaryReader<'_>) -> Result<Vec<MeshFace>, String> {
    let count = reader.compact_count().map_err(codec)?;
    let mut values = Vec::with_capacity(count);
    for _ in 0..count {
        values.push(mesh_face(reader)?);
    }
    Ok(values)
}

fn mesh_wedge_array(reader: &mut BinaryReader<'_>) -> Result<Vec<MeshWedge>, String> {
    let count = reader.compact_count().map_err(codec)?;
    let mut values = Vec::with_capacity(count);
    for _ in 0..count {
        values.push(mesh_wedge(reader)?);
    }
    Ok(values)
}

fn i32_array(reader: &mut BinaryReader<'_>) -> Result<Vec<i32>, String> {
    let count = reader.compact_count().map_err(codec)?;
    let mut values = Vec::with_capacity(count);
    for _ in 0..count {
        values.push(reader.i32().map_err(codec)?);
    }
    Ok(values)
}

fn u16_array(reader: &mut BinaryReader<'_>) -> Result<Vec<u16>, String> {
    let count = reader.compact_count().map_err(codec)?;
    let mut values = Vec::with_capacity(count);
    for _ in 0..count {
        values.push(reader.u16().map_err(codec)?);
    }
    Ok(values)
}

fn lazy_prefix(reader: &mut BinaryReader<'_>) -> Result<i32, String> {
    reader.i32().map_err(codec)
}

fn lazy_vectors(reader: &mut BinaryReader<'_>) -> Result<LazyArray<Vector3>, String> {
    let serialized_end_offset = lazy_prefix(reader)?;
    let count = reader.compact_count().map_err(codec)?;
    let mut values = Vec::with_capacity(count);
    for _ in 0..count {
        values.push(vector(reader)?);
    }
    Ok(LazyArray {
        serialized_end_offset,
        values,
    })
}

fn lazy_wedges(reader: &mut BinaryReader<'_>) -> Result<LazyArray<MeshWedge>, String> {
    let serialized_end_offset = lazy_prefix(reader)?;
    let count = reader.compact_count().map_err(codec)?;
    let mut values = Vec::with_capacity(count);
    for _ in 0..count {
        values.push(mesh_wedge(reader)?);
    }
    Ok(LazyArray {
        serialized_end_offset,
        values,
    })
}

fn lazy_faces(reader: &mut BinaryReader<'_>) -> Result<LazyArray<MeshFace>, String> {
    let serialized_end_offset = lazy_prefix(reader)?;
    let count = reader.compact_count().map_err(codec)?;
    let mut values = Vec::with_capacity(count);
    for _ in 0..count {
        values.push(mesh_face(reader)?);
    }
    Ok(LazyArray {
        serialized_end_offset,
        values,
    })
}

fn lazy_triangles(reader: &mut BinaryReader<'_>) -> Result<LazyArray<MeshTriangle>, String> {
    let serialized_end_offset = lazy_prefix(reader)?;
    let count = reader.compact_count().map_err(codec)?;
    let mut values = Vec::with_capacity(count);
    for _ in 0..count {
        values.push(MeshTriangle {
            wedge_indices: [
                reader.u16().map_err(codec)?,
                reader.u16().map_err(codec)?,
                reader.u16().map_err(codec)?,
            ],
            material_index: reader.u8().map_err(codec)?,
            auxiliary_material_index: reader.u8().map_err(codec)?,
            smoothing_groups: reader.u32().map_err(codec)?,
        });
    }
    Ok(LazyArray {
        serialized_end_offset,
        values,
    })
}

fn lazy_influences(reader: &mut BinaryReader<'_>) -> Result<LazyArray<VertexInfluence>, String> {
    let serialized_end_offset = lazy_prefix(reader)?;
    let count = reader.compact_count().map_err(codec)?;
    let mut values = Vec::with_capacity(count);
    for _ in 0..count {
        values.push(VertexInfluence {
            weight: reader.f32().map_err(codec)?,
            point_index: reader.u16().map_err(codec)?,
            bone_index: reader.u16().map_err(codec)?,
        });
    }
    Ok(LazyArray {
        serialized_end_offset,
        values,
    })
}

fn lazy_u16(reader: &mut BinaryReader<'_>) -> Result<LazyArray<u16>, String> {
    Ok(LazyArray {
        serialized_end_offset: lazy_prefix(reader)?,
        values: u16_array(reader)?,
    })
}

fn mesh_wedge(reader: &mut BinaryReader<'_>) -> Result<MeshWedge, String> {
    Ok(MeshWedge {
        vertex_index: reader.u16().map_err(codec)?,
        u: reader.f32().map_err(codec)?,
        v: reader.f32().map_err(codec)?,
    })
}

fn mesh_face(reader: &mut BinaryReader<'_>) -> Result<MeshFace, String> {
    Ok(MeshFace {
        wedge_indices: [
            reader.u16().map_err(codec)?,
            reader.u16().map_err(codec)?,
            reader.u16().map_err(codec)?,
        ],
        material_index: reader.u16().map_err(codec)?,
    })
}

fn compact_object_link(
    reader: &mut BinaryReader<'_>,
    context: &NativeContext<'_>,
    field: &str,
) -> Result<ObjectLink, String> {
    let wire_value = reader.compact_int().map_err(codec)?.get();
    let reference = ObjectRef::checked(wire_value, context.imports, context.exports, field)?;
    Ok(ObjectLink {
        reference: reference.into(),
        wire_value,
        path: (context.resolve)(reference),
    })
}

fn object_links(
    reader: &mut BinaryReader<'_>,
    context: &NativeContext<'_>,
    field: &str,
) -> Result<Vec<ObjectLink>, String> {
    let count = reader.compact_count().map_err(codec)?;
    let mut links = Vec::with_capacity(count);
    for _ in 0..count {
        links.push(compact_object_link(reader, context, field)?);
    }
    Ok(links)
}

fn name_array(
    reader: &mut BinaryReader<'_>,
    context: &NativeContext<'_>,
) -> Result<Vec<String>, String> {
    let count = reader.compact_count().map_err(codec)?;
    let mut names = Vec::with_capacity(count);
    for _ in 0..count {
        names.push(compact_name(reader, context)?);
    }
    Ok(names)
}

fn mesh_animation(
    reader: &mut BinaryReader<'_>,
    context: &NativeContext<'_>,
) -> Result<MeshAnimation, String> {
    let version = reader.i32().map_err(codec)?;
    let bone_count = reader.compact_count().map_err(codec)?;
    let mut bones = Vec::with_capacity(bone_count);
    for _ in 0..bone_count {
        bones.push(AnimationBone {
            name: compact_name(reader, context)?,
            flags: reader.u32().map_err(codec)?,
            parent_index: reader.i32().map_err(codec)?,
        });
    }

    let moves_end = reader.i32().map_err(codec)?;
    let move_count = reader.compact_count().map_err(codec)?;
    let mut moves = Vec::with_capacity(move_count);
    for _ in 0..move_count {
        let move_end = reader.i32().map_err(codec)?;
        moves.push(animation_move(reader, move_end)?);
        validate_package_offset(reader, move_end, context, "animation move")?;
    }
    validate_package_offset(reader, moves_end, context, "animation moves")?;

    let sequence_count = reader.compact_count().map_err(codec)?;
    let mut sequences = Vec::with_capacity(sequence_count);
    for _ in 0..sequence_count {
        let unknown_28 = reader.f32().map_err(codec)?;
        let name = compact_name(reader, context)?;
        let group_count = reader.compact_count().map_err(codec)?;
        let mut groups = Vec::with_capacity(group_count);
        for _ in 0..group_count {
            groups.push(compact_name(reader, context)?);
        }
        let start_frame = reader.i32().map_err(codec)?;
        let frame_count = reader.i32().map_err(codec)?;
        let notify_count = reader.compact_count().map_err(codec)?;
        let mut notifications = Vec::with_capacity(notify_count);
        for _ in 0..notify_count {
            let notify_frame = f32::from_bits(reader.u32().map_err(codec)?);
            let function = compact_name(reader, context)?;
            let wire_value = reader.compact_int().map_err(codec)?.get();
            let reference = ObjectRef::checked(
                wire_value,
                context.imports,
                context.exports,
                "animation notification",
            )?;
            let lineage_text = (context.version.archive >= 131)
                .then(|| utf16_string(reader))
                .transpose()?;
            notifications.push(AnimationNotification {
                notify_frame,
                function,
                notify: ObjectLink {
                    reference: reference.into(),
                    wire_value,
                    path: (context.resolve)(reference),
                },
                lineage_text,
            });
        }
        let rate = reader.f32().map_err(codec)?;
        let lineage = lineage_sequence_data(reader, context)?;
        sequences.push(AnimationSequence {
            unknown_28,
            name,
            groups,
            start_frame,
            frame_count,
            rate,
            notifications,
            lineage,
        });
    }
    Ok(MeshAnimation {
        version,
        bones,
        moves_end_offset: moves_end,
        moves,
        sequences,
    })
}

fn animation_move(
    reader: &mut BinaryReader<'_>,
    serialized_end_offset: i32,
) -> Result<AnimationMove, String> {
    let root_speed = vector(reader)?;
    let track_time = reader.f32().map_err(codec)?;
    let start_bone = reader.i32().map_err(codec)?;
    let flags = reader.u32().map_err(codec)?;
    let bone_indices = i32_array(reader)?;
    let track_count = reader.compact_count().map_err(codec)?;
    let mut tracks = Vec::with_capacity(track_count);
    for _ in 0..track_count {
        tracks.push(analog_track(reader)?);
    }
    let root_track = analog_track(reader)?;
    Ok(AnimationMove {
        serialized_end_offset,
        root_speed,
        track_time,
        start_bone,
        flags,
        bone_indices,
        tracks,
        root_track,
    })
}

fn analog_track(reader: &mut BinaryReader<'_>) -> Result<AnalogTrack, String> {
    Ok(AnalogTrack {
        flags: reader.u32().map_err(codec)?,
        orientations: quaternion_array(reader)?,
        positions: vector_array(reader)?,
        times: f32_array(reader)?,
    })
}

fn compact_name(
    reader: &mut BinaryReader<'_>,
    context: &NativeContext<'_>,
) -> Result<String, String> {
    let index = reader.compact_int().map_err(codec)?.get();
    Ok((context.resolve_name)(index))
}

fn validate_package_offset(
    reader: &mut BinaryReader<'_>,
    package_offset: i32,
    context: &NativeContext<'_>,
    field: &str,
) -> Result<(), String> {
    let package_offset = usize::try_from(package_offset).map_err(|_| "negative motion offset")?;
    let target = package_offset
        .checked_sub(context.object_offset)
        .ok_or("motion offset precedes object data")?;
    if reader.position() != target {
        return Err(format!(
            "{field} ended at object offset {}, expected {target}",
            reader.position()
        ));
    }
    Ok(())
}

fn lineage_sequence_data(
    reader: &mut BinaryReader<'_>,
    context: &NativeContext<'_>,
) -> Result<LineageSequenceData, String> {
    let version = context.version;
    let unknown_2c = reader.i32().map_err(codec)?;
    let unknown_30 = reader.i32().map_err(codec)?;
    let unknown_34 = (version.license >= 2)
        .then(|| reader.i32().map_err(codec))
        .transpose()?;
    let object = compact_object_link(reader, context, "animation sequence object")?;
    let unknown_3c = (version.license >= 0x14)
        .then(|| reader.i32().map_err(codec))
        .transpose()?;
    let unknown_40 = (version.license >= 0x19)
        .then(|| reader.i32().map_err(codec))
        .transpose()?;
    let extension = if version.license >= 0x1b {
        let flags = Some(reader.u8().map_err(codec)?);
        let pairs = lineage_pairs(reader)?;
        let nested_count = reader.compact_count().map_err(codec)?;
        let mut nested_pairs = Vec::with_capacity(nested_count);
        for _ in 0..nested_count {
            nested_pairs.push(LineageNestedPairs {
                value: reader.i32().map_err(codec)?,
                pairs: lineage_pairs(reader)?,
            });
        }
        Some(LineageSequenceExtension {
            flags,
            pairs,
            nested_pairs,
            unknown_1c: Some(reader.i32().map_err(codec)?),
            unknown_20: Some(reader.i32().map_err(codec)?),
            trailing_pairs: lineage_pairs(reader)?,
        })
    } else if version.license == 0x1a {
        Some(LineageSequenceExtension {
            flags: None,
            pairs: lineage_pairs(reader)?,
            nested_pairs: Vec::new(),
            unknown_1c: None,
            unknown_20: None,
            trailing_pairs: Vec::new(),
        })
    } else {
        None
    };
    Ok(LineageSequenceData {
        unknown_2c,
        unknown_30,
        unknown_34,
        object,
        unknown_3c,
        unknown_40,
        extension,
    })
}

fn lineage_pairs(reader: &mut BinaryReader<'_>) -> Result<Vec<LineagePair>, String> {
    let count = reader.compact_count().map_err(codec)?;
    let mut pairs = Vec::with_capacity(count);
    for _ in 0..count {
        pairs.push(LineagePair {
            first: reader.i32().map_err(codec)?,
            second: reader.i32().map_err(codec)?,
        });
    }
    Ok(pairs)
}

fn static_mesh_instance(
    reader: &mut BinaryReader<'_>,
    data: &[u8],
    context: &NativeContext<'_>,
) -> Result<StaticMeshInstance, String> {
    let start = reader.position();
    let color_count = reader.compact_count().map_err(codec)?;
    let mut color_stream = Vec::with_capacity(color_count);
    for _ in 0..color_count {
        color_stream.push(Color {
            red: reader.u8().map_err(codec)?,
            green: reader.u8().map_err(codec)?,
            blue: reader.u8().map_err(codec)?,
            alpha: reader.u8().map_err(codec)?,
        });
    }
    let color_revision = reader.i32().map_err(codec)?;
    let scene_lights = lights(reader, context)?;
    let environment_lights = if context.version.has_environment_lights() {
        lights(reader, context)?
    } else {
        Vec::new()
    };
    let mut unknown_indices = Vec::new();
    if context.version.has_static_mesh_unknown_indices() {
        unknown_indices.push(reader.compact_int().map_err(codec)?.get());
        unknown_indices.push(reader.compact_int().map_err(codec)?.get());
    }
    Ok(StaticMeshInstance {
        color_stream,
        color_revision,
        scene_lights,
        environment_lights,
        unknown_indices,
        original: data[start..reader.position()].to_vec(),
    })
}

fn lights(
    reader: &mut BinaryReader<'_>,
    context: &NativeContext<'_>,
) -> Result<Vec<StaticMeshLightInfo>, String> {
    let count = reader.compact_count().map_err(codec)?;
    let mut result = Vec::with_capacity(count);
    for _ in 0..count {
        let wire_value = reader.compact_int().map_err(codec)?.get();
        let reference = ObjectRef::checked(
            wire_value,
            context.imports,
            context.exports,
            "static mesh light",
        )?;
        let n = reader.compact_count().map_err(codec)?;
        let mut vertex_flags = Vec::with_capacity(n);
        for _ in 0..n {
            vertex_flags.push(reader.u8().map_err(codec)?);
        }
        result.push(StaticMeshLightInfo {
            light: ObjectLink {
                reference: reference.into(),
                wire_value,
                path: (context.resolve)(reference),
            },
            vertex_flags,
            applied: reader.i32().map_err(codec)? != 0,
        });
    }
    Ok(result)
}

fn texture(reader: &mut BinaryReader<'_>, version: PackageVersion) -> Result<Texture, String> {
    let material = optional_lineage_material(reader, version)?;
    let count = reader.compact_count().map_err(codec)?;
    let mut mipmaps = Vec::with_capacity(count);
    for _ in 0..count {
        let serialized_end_offset = reader.u32().map_err(codec)?;
        let byte_count = reader.compact_count().map_err(codec)?;
        let data = reader.take(byte_count).map_err(codec)?.to_vec();
        mipmaps.push(TextureMip {
            serialized_end_offset,
            data,
            width: reader.i32().map_err(codec)?,
            height: reader.i32().map_err(codec)?,
            width_bits: reader.u8().map_err(codec)?,
            height_bits: reader.u8().map_err(codec)?,
        });
    }
    Ok(Texture { material, mipmaps })
}

fn optional_lineage_material(
    reader: &mut BinaryReader<'_>,
    version: PackageVersion,
) -> Result<Option<LineageMaterial>, String> {
    if version.archive >= 123 && version.license >= 37 {
        lineage_material(reader, version).map(Some)
    } else {
        Ok(None)
    }
}

fn lineage_material(
    reader: &mut BinaryReader<'_>,
    version: PackageVersion,
) -> Result<LineageMaterial, String> {
    if version.archive < 123 || version.license < 37 {
        return Err(format!(
            "unsupported material native layout for archive {} license {}",
            version.archive, version.license
        ));
    }
    let flags = [reader.u8().map_err(codec)?, reader.u8().map_err(codec)?];
    let pass_count = if version.archive < 129 { 1 } else { 5 };
    let mut passes = Vec::with_capacity(pass_count);
    for _ in 0..pass_count {
        let flag_count = if version.archive < 130 { 2 } else { 6 };
        let mut pass_flags = Vec::with_capacity(flag_count);
        for _ in 0..flag_count {
            pass_flags.push(reader.u8().map_err(codec)?);
        }
        let legacy_value = (version.archive < 130)
            .then(|| reader.i32().map_err(codec))
            .transpose()?;
        passes.push(LineageShaderPass {
            flags: pass_flags,
            legacy_value,
            blend_mode: reader.i32().map_err(codec)?,
            fog_color: reader.i32().map_err(codec)?,
        });
    }
    let mut fade_colors = [0; 8];
    for color in &mut fade_colors {
        *color = reader.u8().map_err(codec)?;
    }
    let fade_period = reader.i32().map_err(codec)?;
    let fade_phase = reader.i32().map_err(codec)?;
    let color_fade_type = reader.i32().map_err(codec)?;
    let stage_count = reader.compact_count().map_err(codec)?;
    let mut stages = Vec::with_capacity(stage_count);
    for _ in 0..stage_count {
        let name = unreal_string(reader)?;
        let texture_count = reader.compact_count().map_err(codec)?;
        let mut textures = Vec::with_capacity(texture_count);
        for _ in 0..texture_count {
            textures.push(unreal_string(reader)?);
        }
        stages.push(LineageMaterialStage { name, textures });
    }
    let shader_code = unreal_string(reader)?;
    let material_code_version = [reader.u16().map_err(codec)?, reader.u16().map_err(codec)?];
    Ok(LineageMaterial {
        shader_property: LineageShaderProperty {
            flags,
            passes,
            fade_colors,
            fade_period,
            fade_phase,
            color_fade_type,
            stages,
        },
        shader_code,
        material_code_version,
    })
}

fn terrain_info(
    reader: &mut BinaryReader<'_>,
    version: PackageVersion,
) -> Result<TerrainInfo, String> {
    let count = reader.compact_count().map_err(codec)?;
    let mut sectors = Vec::with_capacity(count);
    for _ in 0..count {
        sectors.push(reader.compact_int().map_err(codec)?.get());
    }
    let vertices = if version.license < 17 {
        vector_array(reader)?
    } else {
        Vec::new()
    };
    let sectors_x = reader.i32().map_err(codec)?;
    let sectors_y = reader.i32().map_err(codec)?;
    let face_normals = if version.license < 17 {
        let count = reader.compact_count().map_err(codec)?;
        let mut values = Vec::with_capacity(count);
        for _ in 0..count {
            values.push(TerrainNormalPair {
                first: vector(reader)?,
                second: vector(reader)?,
            });
        }
        values
    } else {
        Vec::new()
    };
    let to_world = coordinates_value(reader)?;
    let to_height_map = coordinates_value(reader)?;
    let heightmap_x = reader.i32().map_err(codec)?;
    let heightmap_y = reader.i32().map_err(codec)?;
    let vertex_colors = if version.has_terrain_vertex_colors() {
        color_array(reader)?
    } else {
        Vec::new()
    };
    Ok(TerrainInfo {
        sectors,
        vertices,
        sectors_x,
        sectors_y,
        face_normals,
        to_world,
        to_height_map,
        heightmap_x,
        heightmap_y,
        vertex_colors,
    })
}

fn terrain_sector(
    reader: &mut BinaryReader<'_>,
    context: &NativeContext<'_>,
) -> Result<TerrainSector, String> {
    let info = compact_object_link(reader, context, "terrain info")?;
    let quads_x = reader.i32().map_err(codec)?;
    let quads_y = reader.i32().map_err(codec)?;
    let offset_x = reader.i32().map_err(codec)?;
    let offset_y = reader.i32().map_err(codec)?;
    let bounding_box = BoxBounds {
        min: vector(reader)?,
        max: vector(reader)?,
        valid: reader.u8().map_err(codec)? != 0,
    };
    let count = reader.compact_count().map_err(codec)?;
    let mut light_infos = Vec::with_capacity(count);
    for _ in 0..count {
        let light = compact_object_link(reader, context, "terrain light")?;
        let byte_count = reader.compact_count().map_err(codec)?;
        light_infos.push(TerrainLightInfo {
            light,
            data: reader.take(byte_count).map_err(codec)?.to_vec(),
        });
    }
    Ok(TerrainSector {
        info,
        quads_x,
        quads_y,
        offset_x,
        offset_y,
        bounding_box,
        light_infos,
    })
}

fn codec(error: crate::codec::CodecError) -> String {
    format!("native codec error: {error:?}")
}

fn remaining_bytes(reader: &mut BinaryReader<'_>) -> Result<Vec<u8>, String> {
    Ok(reader.take(reader.remaining()).map_err(codec)?.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lineage_skeletal_mesh_decodes_inherited_and_skeletal_fields() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&[0; 24]); // box vectors
        bytes.push(1); // valid box
        bytes.extend_from_slice(&[0; 16]); // sphere
        bytes.extend_from_slice(&7i32.to_le_bytes());
        bytes.extend_from_slice(&0i32.to_le_bytes()); // vertex count
        bytes.extend_from_slice(&[0, 0]); // source vertices and materials
        bytes.extend_from_slice(&[0; 36]); // scale, origin, rotation
        bytes.extend_from_slice(&[0; 5]); // inherited mesh arrays
        bytes.extend_from_slice(&[0; 24]); // LOD parameters
        bytes.extend_from_slice(&[0; 4]); // has impostor
        bytes.push(0); // impostor material
        bytes.extend_from_slice(&[0; 52]); // impostor transform, color, modes
        bytes.extend_from_slice(&[0; 8]); // tessellation and Lineage field
        bytes.extend_from_slice(&[0; 2]); // version 6 and 7 padding
        bytes.push(0); // secondary points
        bytes.push(1); // one bone
        bytes.push(0); // bone name
        bytes.extend_from_slice(&[0; 48]); // flags and joint pose
        bytes.extend_from_slice(&0i32.to_le_bytes()); // child count
        bytes.extend_from_slice(&0i32.to_le_bytes()); // parent index
        bytes.push(0); // animation
        bytes.extend_from_slice(&1i32.to_le_bytes()); // skeletal depth
        bytes.extend_from_slice(&[0; 5]); // weights and socket arrays
        bytes.push(0); // no LODs
        bytes.push(0); // legacy object
        for _ in 0..6 {
            bytes.extend_from_slice(&[0; 4]); // lazy-array serialized end offset
            bytes.push(0); // empty lazy array
        }
        bytes.extend_from_slice(&[0; 4]); // Lineage field
        bytes.push(0); // empty Lineage float array
        bytes.extend_from_slice(&[0; 8]); // auth fields

        let context = NativeContext {
            version: PackageVersion::new(133, 40),
            imports: 0,
            exports: 0,
            object_offset: 0,
            resolve_name: &|_| "Root".into(),
            resolve: &|_| String::new(),
        };
        let decoded = NativeObjectRegistry::decode("Engine.SkeletalMesh", &bytes, &context)
            .expect("skeletal mesh should decode");
        let NativeObject::SkeletalMesh(mesh) = decoded.object else {
            panic!("wrong native object type");
        };
        assert_eq!(mesh.mesh_version, 7);
        assert_eq!(mesh.mesh_scale_max, 0.0);
        assert_eq!(mesh.lod_min_vertices, 0);
        assert_eq!(mesh.skeleton[0].name, "Root");
        assert_eq!(mesh.skeletal_depth, 1);
        assert!(mesh.lods.is_empty());
        assert_eq!(decoded.consumed, bytes.len());
    }

    #[test]
    fn level_decodes_actor_references() {
        let bytes = [2, 0, 0, 0, 2, 0, 0, 0, 1, 2];
        let context = NativeContext {
            version: PackageVersion::new(123, 0),
            imports: 0,
            exports: 2,
            object_offset: 0,
            resolve_name: &|index| format!("name:{index}"),
            resolve: &|reference| format!("Actor{}", reference.wire()),
        };
        let decoded = NativeObjectRegistry::decode("Engine.Level", &bytes, &context).unwrap();
        let NativeObject::Level(level) = decoded.object else {
            panic!("wrong native object type");
        };
        assert_eq!(level.actors[0].path, "Actor1");
        assert_eq!(level.actors[1].path, "Actor2");
        assert_eq!(decoded.consumed, bytes.len());
    }

    #[test]
    fn lineage_animation_decodes_notification_frames() {
        let object_offset = 100usize;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&1i32.to_le_bytes()); // animation version
        bytes.push(0); // no bones
        bytes.extend_from_slice(&110i32.to_le_bytes()); // motion data end
        bytes.push(0); // no motion chunks
        bytes.push(1); // one sequence
        bytes.extend_from_slice(&0u32.to_le_bytes()); // f28
        bytes.push(1); // sequence name
        bytes.push(0); // no groups
        bytes.extend_from_slice(&0i32.to_le_bytes()); // start frame
        bytes.extend_from_slice(&30i32.to_le_bytes()); // frame count
        bytes.push(1); // one notification
        bytes.extend_from_slice(&0.5f32.to_le_bytes());
        bytes.push(0); // function name
        bytes.push(1); // notify export reference
        bytes.extend_from_slice(&0i32.to_le_bytes()); // Lineage string length
        bytes.extend_from_slice(&30.0f32.to_le_bytes()); // rate
        bytes.extend_from_slice(&[0; 12]); // f2c, f30, f34
        bytes.push(0); // f38 object reference
        bytes.extend_from_slice(&[0; 8]); // f3c, f40
        bytes.push(0); // f44.f0
        bytes.push(0); // f44.f4
        bytes.push(0); // f44.f10
        bytes.extend_from_slice(&[0; 8]); // f44.f1c, f44.f20
        bytes.push(0); // f44.f24

        let context = NativeContext {
            version: PackageVersion::new(133, 40),
            imports: 0,
            exports: 1,
            object_offset,
            resolve_name: &|index| match index {
                0 => "None".into(),
                1 => "Walk".into(),
                _ => format!("#{index}"),
            },
            resolve: &|reference| format!("Notify{}", reference.wire()),
        };
        let decoded = NativeObjectRegistry::decode("Engine.MeshAnimation", &bytes, &context)
            .expect("animation should decode");
        let NativeObject::MeshAnimation(animation) = decoded.object else {
            panic!("wrong native object type");
        };
        assert_eq!(animation.sequences[0].name, "Walk");
        assert_eq!(animation.sequences[0].notifications[0].notify_frame, 0.5);
        assert_eq!(
            animation.sequences[0].notifications[0].notify.path,
            "Notify1"
        );
        assert_eq!(decoded.consumed, bytes.len());
    }

    #[test]
    fn lineage_texture_decodes_material_header_and_mipmaps() {
        let mut bytes = vec![0, 1]; // shader-property flags
        for pass in 0..5u8 {
            bytes.extend_from_slice(&[pass; 6]);
            bytes.extend_from_slice(&(i32::from(pass) + 10).to_le_bytes());
            bytes.extend_from_slice(&(i32::from(pass) + 20).to_le_bytes());
        }
        bytes.extend_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]); // fade colors
        bytes.extend_from_slice(&[0; 12]); // fade values
        bytes.push(0); // no material stages
        bytes.push(0); // empty shader code
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&820u16.to_le_bytes());
        let material_bytes = bytes.clone();
        bytes.push(1); // one mipmap
        bytes.extend_from_slice(&1234u32.to_le_bytes()); // serialized end offset
        bytes.push(2); // two data bytes
        bytes.extend_from_slice(&[0xaa, 0xbb]);
        bytes.extend_from_slice(&4i32.to_le_bytes());
        bytes.extend_from_slice(&2i32.to_le_bytes());
        bytes.extend_from_slice(&[2, 1]);

        let context = NativeContext {
            version: PackageVersion::new(133, 40),
            imports: 0,
            exports: 0,
            object_offset: 0,
            resolve_name: &|index| format!("name:{index}"),
            resolve: &|_| String::new(),
        };
        let decoded = NativeObjectRegistry::decode("Engine.Texture", &bytes, &context).unwrap();
        let NativeObject::Texture(texture) = decoded.object else {
            panic!("wrong native object type");
        };
        let material = texture.material.as_ref().unwrap();
        assert_eq!(material.shader_property.flags, [0, 1]);
        assert_eq!(material.shader_property.passes.len(), 5);
        assert_eq!(material.material_code_version, [1, 820]);
        assert_eq!(texture.mipmaps[0].data, [0xaa, 0xbb]);
        assert_eq!(texture.mipmaps[0].width, 4);
        assert_eq!(texture.mipmaps[0].height, 2);
        assert_eq!(decoded.consumed, bytes.len());

        let decoded =
            NativeObjectRegistry::decode("Engine.TexEnvMap", &material_bytes, &context).unwrap();
        let NativeObject::TexEnvMap(environment) = decoded.object else {
            panic!("wrong native object type");
        };
        assert_eq!(
            environment.material.unwrap().material_code_version,
            [1, 820]
        );
        assert_eq!(decoded.consumed, material_bytes.len());
    }

    #[test]
    fn static_mesh_instance_decodes_typed_colors_and_lights() {
        let bytes = [
            2, 1, 2, 3, 4, 5, 6, 7, 8, // two colors
            1, 0, 0, 0, // revision
            1, // one scene light
            1, // export reference 1
            2, 0xaa, 0xbb, // two vertex flags
            1, 0, 0, 0, // applied
        ];
        let context = NativeContext {
            version: PackageVersion::new(123, 0),
            imports: 0,
            exports: 1,
            object_offset: 0,
            resolve_name: &|index| format!("name:{index}"),
            resolve: &|reference| format!("resolved:{}", reference.wire()),
        };
        let decoded =
            NativeObjectRegistry::decode("Engine.StaticMeshInstance", &bytes, &context).unwrap();
        let NativeObject::StaticMeshInstance(mesh) = decoded.object else {
            panic!("wrong native object type");
        };
        assert_eq!(
            mesh.color_stream[1],
            Color {
                red: 5,
                green: 6,
                blue: 7,
                alpha: 8
            }
        );
        assert_eq!(mesh.color_revision, 1);
        assert_eq!(mesh.scene_lights[0].light.wire_value, 1);
        assert_eq!(mesh.scene_lights[0].vertex_flags, [0xaa, 0xbb]);
        assert!(mesh.scene_lights[0].applied);
        assert_eq!(mesh.original, bytes);
        assert_eq!(decoded.consumed, bytes.len());
    }

    #[test]
    fn static_mesh_and_legacy_bsp_types_consume_their_native_payloads() {
        let context = NativeContext {
            version: PackageVersion::new(123, 0),
            imports: 0,
            exports: 0,
            object_offset: 0,
            resolve_name: &|index| format!("name:{index}"),
            resolve: &|_| String::new(),
        };

        let mut mesh_bytes = vec![0; 25 + 16]; // inherited primitive bounds
        mesh_bytes.push(0); // no sections
        mesh_bytes.extend_from_slice(&[0; 25]); // repeated bounding box
        for _ in 0..3 {
            mesh_bytes.push(0); // empty vertex/color/alpha stream
            mesh_bytes.extend_from_slice(&0i32.to_le_bytes()); // revision
        }
        mesh_bytes.push(0); // no UV streams
        for _ in 0..2 {
            mesh_bytes.push(0); // empty index buffer
            mesh_bytes.extend_from_slice(&0i32.to_le_bytes()); // revision
        }
        mesh_bytes.push(0); // null collision object
        mesh_bytes.extend_from_slice(&[1, 2, 3]); // retained collision payload
        let decoded =
            NativeObjectRegistry::decode("Engine.StaticMesh", &mesh_bytes, &context).unwrap();
        let NativeObject::StaticMesh(mesh) = decoded.object else {
            panic!("wrong native object type");
        };
        assert_eq!(mesh.collision_data, [1, 2, 3]);
        assert_eq!(decoded.consumed, mesh_bytes.len());

        let mut model_bytes = vec![0; 25 + 16];
        model_bytes.extend_from_slice(&[4, 5]);
        let decoded = NativeObjectRegistry::decode("Engine.Model", &model_bytes, &context).unwrap();
        let NativeObject::Model(model) = decoded.object else {
            panic!("wrong native object type");
        };
        assert_eq!(model.serialized_bsp, [4, 5]);

        let mut polys_bytes = 1i32.to_le_bytes().to_vec();
        polys_bytes.extend_from_slice(&1i32.to_le_bytes());
        polys_bytes.extend_from_slice(&[6, 7]);
        let decoded = NativeObjectRegistry::decode("Engine.Polys", &polys_bytes, &context).unwrap();
        let NativeObject::Polys(polys) = decoded.object else {
            panic!("wrong native object type");
        };
        assert_eq!(polys.database_count, 1);
        assert_eq!(polys.serialized_polygons, [6, 7]);
    }

    #[test]
    fn unknown_decoder_consumes_nothing() {
        let context = NativeContext {
            version: PackageVersion::new(123, 0),
            imports: 0,
            exports: 0,
            object_offset: 0,
            resolve_name: &|index| format!("name:{index}"),
            resolve: &|_| String::new(),
        };
        let decoded = NativeObjectRegistry::decode("Game.Unknown", &[1, 2, 3], &context).unwrap();
        assert!(matches!(decoded.object, NativeObject::Unknown));
        assert_eq!(decoded.consumed, 0);
    }
}
