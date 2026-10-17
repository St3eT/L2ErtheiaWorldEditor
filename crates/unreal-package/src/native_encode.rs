use crate::codec::BinaryWriter;
use crate::model::PackageVersion;
use crate::native::*;

pub(crate) struct Context<'a> {
    pub version: PackageVersion,
    pub object_offset: usize,
    pub name_index: &'a dyn Fn(&str) -> Result<i32, String>,
    pub object_wire: &'a dyn Fn(&str) -> Result<i32, String>,
}

pub(crate) fn encode(object: &NativeObject, context: &Context<'_>) -> Result<Vec<u8>, String> {
    let mut writer = BinaryWriter::default();
    match object {
        NativeObject::MeshAnimation(value) => mesh_animation(&mut writer, value, context)?,
        NativeObject::SkeletalMesh(value) => skeletal_mesh(&mut writer, value, context)?,
        _ => return Err("edited native type does not have a binary writer yet".into()),
    }
    Ok(writer.into_bytes())
}

fn absolute(writer: &BinaryWriter, context: &Context<'_>) -> Result<i32, String> {
    i32::try_from(context.object_offset + writer.position())
        .map_err(|_| "native object offset exceeds i32".into())
}

fn count(writer: &mut BinaryWriter, length: usize) -> Result<(), String> {
    writer.compact(i32::try_from(length).map_err(|_| "array is too large")?);
    Ok(())
}

fn name(writer: &mut BinaryWriter, value: &str, context: &Context<'_>) -> Result<(), String> {
    writer.compact((context.name_index)(value)?);
    Ok(())
}

fn object(
    writer: &mut BinaryWriter,
    value: &ObjectLink,
    context: &Context<'_>,
) -> Result<(), String> {
    writer.compact((context.object_wire)(&value.path)?);
    Ok(())
}

fn vector(writer: &mut BinaryWriter, value: &Vector3) {
    writer.f32(value.x);
    writer.f32(value.y);
    writer.f32(value.z);
}

fn quaternion(writer: &mut BinaryWriter, value: &Quaternion) {
    writer.f32(value.x);
    writer.f32(value.y);
    writer.f32(value.z);
    writer.f32(value.w);
}

fn rotator(writer: &mut BinaryWriter, value: &Rotator) {
    writer.i32(value.pitch);
    writer.i32(value.yaw);
    writer.i32(value.roll);
}

fn coordinates(writer: &mut BinaryWriter, value: &Coordinates) {
    vector(writer, &value.origin);
    vector(writer, &value.x_axis);
    vector(writer, &value.y_axis);
    vector(writer, &value.z_axis);
}

fn box_bounds(writer: &mut BinaryWriter, value: &BoxBounds) {
    vector(writer, &value.min);
    vector(writer, &value.max);
    writer.u8(u8::from(value.valid));
}

fn vector_array(writer: &mut BinaryWriter, values: &[Vector3]) -> Result<(), String> {
    count(writer, values.len())?;
    for value in values {
        vector(writer, value);
    }
    Ok(())
}

fn quaternion_array(writer: &mut BinaryWriter, values: &[Quaternion]) -> Result<(), String> {
    count(writer, values.len())?;
    for value in values {
        quaternion(writer, value);
    }
    Ok(())
}

fn i32_array(writer: &mut BinaryWriter, values: &[i32]) -> Result<(), String> {
    count(writer, values.len())?;
    for &value in values {
        writer.i32(value);
    }
    Ok(())
}

fn u32_array(writer: &mut BinaryWriter, values: &[u32]) -> Result<(), String> {
    count(writer, values.len())?;
    for &value in values {
        writer.u32(value);
    }
    Ok(())
}

fn u16_array(writer: &mut BinaryWriter, values: &[u16]) -> Result<(), String> {
    count(writer, values.len())?;
    for &value in values {
        writer.u16(value);
    }
    Ok(())
}

fn f32_array(writer: &mut BinaryWriter, values: &[f32]) -> Result<(), String> {
    count(writer, values.len())?;
    for &value in values {
        writer.f32(value);
    }
    Ok(())
}

fn names(
    writer: &mut BinaryWriter,
    values: &[String],
    context: &Context<'_>,
) -> Result<(), String> {
    count(writer, values.len())?;
    for value in values {
        name(writer, value, context)?;
    }
    Ok(())
}

fn objects(
    writer: &mut BinaryWriter,
    values: &[ObjectLink],
    context: &Context<'_>,
) -> Result<(), String> {
    count(writer, values.len())?;
    for value in values {
        object(writer, value, context)?;
    }
    Ok(())
}

fn utf16_string(writer: &mut BinaryWriter, value: &str) -> Result<(), String> {
    let mut units: Vec<u16> = value.encode_utf16().collect();
    units.push(0);
    writer.i32(i32::try_from(units.len()).map_err(|_| "UTF-16 string is too long")?);
    for unit in units {
        writer.u16(unit);
    }
    Ok(())
}

fn mesh_animation(
    writer: &mut BinaryWriter,
    value: &MeshAnimation,
    context: &Context<'_>,
) -> Result<(), String> {
    writer.i32(value.version);
    count(writer, value.bones.len())?;
    for bone in &value.bones {
        name(writer, &bone.name, context)?;
        writer.u32(bone.flags);
        writer.i32(bone.parent_index);
    }

    let moves_end = writer.position();
    writer.i32(0);
    count(writer, value.moves.len())?;
    for movement in &value.moves {
        let move_end = writer.position();
        writer.i32(0);
        animation_move(writer, movement)?;
        writer.patch_i32(move_end, absolute(writer, context)?);
    }
    writer.patch_i32(moves_end, absolute(writer, context)?);

    count(writer, value.sequences.len())?;
    for sequence in &value.sequences {
        writer.f32(sequence.unknown_28);
        name(writer, &sequence.name, context)?;
        names(writer, &sequence.groups, context)?;
        writer.i32(sequence.start_frame);
        writer.i32(sequence.frame_count);
        count(writer, sequence.notifications.len())?;
        for notification in &sequence.notifications {
            writer.f32(notification.notify_frame);
            name(writer, &notification.function, context)?;
            object(writer, &notification.notify, context)?;
            if context.version.archive >= 131 {
                utf16_string(
                    writer,
                    notification.lineage_text.as_deref().unwrap_or_default(),
                )?;
            }
        }
        writer.f32(sequence.rate);
        lineage_sequence(writer, &sequence.lineage, context)?;
    }
    Ok(())
}

fn animation_move(writer: &mut BinaryWriter, value: &AnimationMove) -> Result<(), String> {
    vector(writer, &value.root_speed);
    writer.f32(value.track_time);
    writer.i32(value.start_bone);
    writer.u32(value.flags);
    i32_array(writer, &value.bone_indices)?;
    count(writer, value.tracks.len())?;
    for track in &value.tracks {
        analog_track(writer, track)?;
    }
    analog_track(writer, &value.root_track)
}

fn analog_track(writer: &mut BinaryWriter, value: &AnalogTrack) -> Result<(), String> {
    writer.u32(value.flags);
    quaternion_array(writer, &value.orientations)?;
    vector_array(writer, &value.positions)?;
    f32_array(writer, &value.times)
}

fn lineage_sequence(
    writer: &mut BinaryWriter,
    value: &LineageSequenceData,
    context: &Context<'_>,
) -> Result<(), String> {
    let version = context.version;
    writer.i32(value.unknown_2c);
    writer.i32(value.unknown_30);
    if version.license >= 2 {
        writer.i32(
            value
                .unknown_34
                .ok_or("missing lineage sequence unknown34")?,
        );
    }
    object(writer, &value.object, context)?;
    if version.license >= 0x14 {
        writer.i32(
            value
                .unknown_3c
                .ok_or("missing lineage sequence unknown3c")?,
        );
    }
    if version.license >= 0x19 {
        writer.i32(
            value
                .unknown_40
                .ok_or("missing lineage sequence unknown40")?,
        );
    }
    if version.license >= 0x1b {
        let extension = value
            .extension
            .as_ref()
            .ok_or("missing lineage sequence extension")?;
        writer.u8(extension.flags.ok_or("missing lineage sequence flags")?);
        pairs(writer, &extension.pairs)?;
        count(writer, extension.nested_pairs.len())?;
        for nested in &extension.nested_pairs {
            writer.i32(nested.value);
            pairs(writer, &nested.pairs)?;
        }
        writer.i32(
            extension
                .unknown_1c
                .ok_or("missing lineage sequence unknown1c")?,
        );
        writer.i32(
            extension
                .unknown_20
                .ok_or("missing lineage sequence unknown20")?,
        );
        pairs(writer, &extension.trailing_pairs)?;
    } else if version.license == 0x1a {
        pairs(
            writer,
            &value
                .extension
                .as_ref()
                .ok_or("missing lineage sequence extension")?
                .pairs,
        )?;
    }
    Ok(())
}

fn pairs(writer: &mut BinaryWriter, values: &[LineagePair]) -> Result<(), String> {
    count(writer, values.len())?;
    for value in values {
        writer.i32(value.first);
        writer.i32(value.second);
    }
    Ok(())
}

fn skeletal_mesh(
    writer: &mut BinaryWriter,
    value: &SkeletalMesh,
    context: &Context<'_>,
) -> Result<(), String> {
    box_bounds(writer, &value.bounding_box);
    vector(writer, &value.bounding_sphere.center);
    writer.f32(value.bounding_sphere.radius);
    writer.i32(value.mesh_version);
    writer.i32(value.vertex_count);
    vector_array(writer, &value.source_vertices)?;
    objects(writer, &value.materials, context)?;
    vector(writer, &value.mesh_scale);
    vector(writer, &value.mesh_origin);
    rotator(writer, &value.rotation_origin);
    u16_array(writer, &value.face_levels)?;
    mesh_faces(writer, &value.inherited_faces)?;
    u16_array(writer, &value.inherited_collapse_wedges)?;
    mesh_wedges(writer, &value.inherited_wedges)?;
    count(writer, value.material_slots.len())?;
    for material in &value.material_slots {
        writer.u32(material.polygon_flags);
        writer.i32(material.texture_index);
    }
    writer.f32(value.mesh_scale_max);
    writer.f32(value.lod_hysteresis);
    writer.f32(value.lod_strength);
    writer.i32(value.lod_min_vertices);
    writer.f32(value.lod_morph);
    writer.f32(value.lod_z_displacement);
    if value.mesh_version >= 3 {
        let impostor = value.impostor.as_ref().ok_or("missing mesh impostor")?;
        writer.i32(impostor.enabled);
        object(writer, &impostor.material, context)?;
        vector(writer, &impostor.location);
        rotator(writer, &impostor.rotation);
        vector(writer, &impostor.scale);
        writer.u32(impostor.color);
        writer.i32(impostor.space_mode);
        writer.i32(impostor.draw_mode);
        writer.i32(impostor.light_mode);
    }
    if value.mesh_version >= 4 {
        writer.f32(
            value
                .skin_tessellation_factor
                .ok_or("missing tessellation factor")?,
        );
    }
    if value.mesh_version >= 5 {
        writer.i32(
            value
                .lineage_lod_unknown
                .ok_or("missing lineage LOD value")?,
        );
    }
    if value.mesh_version >= 6 {
        writer.u8(value
            .lineage_version_6_byte
            .ok_or("missing mesh version 6 byte")?);
    }
    if value.mesh_version >= 7 {
        writer.u8(value
            .lineage_version_7_byte
            .ok_or("missing mesh version 7 byte")?);
    }
    vector_array(writer, &value.secondary_points)?;
    count(writer, value.skeleton.len())?;
    for bone in &value.skeleton {
        name(writer, &bone.name, context)?;
        writer.u32(bone.flags);
        quaternion(writer, &bone.orientation);
        vector(writer, &bone.position);
        writer.f32(bone.length);
        vector(writer, &bone.size);
        writer.i32(bone.child_count);
        writer.i32(bone.parent_index);
    }
    object(writer, &value.animation, context)?;
    writer.i32(value.skeletal_depth);
    count(writer, value.weight_indices.len())?;
    for index in &value.weight_indices {
        u16_array(writer, &index.bone_influence_indices)?;
        writer.i32(index.start_bone_influence);
    }
    count(writer, value.bone_influences.len())?;
    for influence in &value.bone_influences {
        writer.u16(influence.bone_weight);
        writer.u16(influence.bone_index);
    }
    names(
        writer,
        &value
            .sockets
            .iter()
            .map(|socket| socket.alias.clone())
            .collect::<Vec<_>>(),
        context,
    )?;
    names(
        writer,
        &value
            .sockets
            .iter()
            .map(|socket| socket.bone.clone())
            .collect::<Vec<_>>(),
        context,
    )?;
    count(writer, value.sockets.len())?;
    for socket in &value.sockets {
        coordinates(writer, &socket.coordinates);
    }
    count(writer, value.lods.len())?;
    for lod in &value.lods {
        skeletal_lod(writer, lod, context)?;
    }
    object(writer, &value.legacy_object, context)?;
    lazy_vectors(writer, &value.points, context)?;
    lazy_wedges(writer, &value.wedges, context)?;
    lazy_triangles(writer, &value.triangles, context)?;
    lazy_influences(writer, &value.influences, context)?;
    lazy_u16(writer, &value.collapse_wedges, context)?;
    lazy_u16(writer, &value.triangle_points, context)?;
    if context.version.archive >= 118 && context.version.license >= 3 {
        writer.i32(value.lineage_unknown_1.ok_or("missing lineage unknown1")?);
    }
    if context.version.archive >= 123 && context.version.license >= 0x12 {
        f32_array(writer, &value.lineage_floats)?;
    }
    if context.version.archive >= 120 {
        writer.i32(value.lineage_auth_key.ok_or("missing lineage auth key")?);
    }
    if context.version.license >= 0x23 {
        writer.i32(value.lineage_unknown_4.ok_or("missing lineage unknown4")?);
    }
    Ok(())
}

fn skeletal_lod(
    writer: &mut BinaryWriter,
    value: &SkeletalLod,
    context: &Context<'_>,
) -> Result<(), String> {
    u32_array(writer, &value.skinning_data)?;
    count(writer, value.skin_points.len())?;
    for point in &value.skin_points {
        vector(writer, &point.position);
        writer.u32(point.packed_normal);
    }
    writer.i32(value.soft_wedge_count);
    sections(writer, &value.soft_sections, context.version)?;
    sections(writer, &value.rigid_sections, context.version)?;
    index_buffer(writer, &value.soft_indices)?;
    index_buffer(writer, &value.rigid_indices)?;
    writer.i32(value.vertex_stream_revision);
    writer.i32(value.vertex_stream_unknown_1);
    writer.i32(value.vertex_stream_unknown_2);
    count(writer, value.rigid_vertices.len())?;
    for vertex in &value.rigid_vertices {
        mesh_vertex(writer, vertex);
    }
    lazy_influences(writer, &value.influences, context)?;
    lazy_wedges(writer, &value.wedges, context)?;
    lazy_faces(writer, &value.faces, context)?;
    lazy_vectors(writer, &value.points, context)?;
    writer.f32(value.distance_factor);
    writer.f32(value.hysteresis);
    writer.i32(value.shared_vertex_count);
    writer.i32(value.max_influences);
    writer.i32(value.unknown_114);
    writer.i32(value.unknown_118);
    writer.i32(i32::from(value.use_new_wedges));
    count(writer, value.lineage_vertices.len())?;
    for vertex in &value.lineage_vertices {
        lineage_vertex(writer, vertex);
    }
    Ok(())
}

fn sections(
    writer: &mut BinaryWriter,
    values: &[SkeletalSection],
    version: PackageVersion,
) -> Result<(), String> {
    count(writer, values.len())?;
    for value in values {
        writer.u16(value.material_index);
        writer.u16(value.min_stream_index);
        writer.u16(value.min_wedge_index);
        writer.u16(value.max_wedge_index);
        writer.u16(value.stream_index_count);
        writer.u16(value.bone_index);
        writer.u16(value.unknown_e);
        writer.u16(value.first_face);
        writer.u16(value.face_count);
        if version.license >= 0x1c {
            i32_array(writer, &value.bone_map)?;
        }
    }
    Ok(())
}

fn index_buffer(writer: &mut BinaryWriter, value: &IndexBuffer) -> Result<(), String> {
    u16_array(writer, &value.indices)?;
    writer.i32(value.revision);
    Ok(())
}

fn mesh_vertex(writer: &mut BinaryWriter, value: &MeshVertex) {
    vector(writer, &value.position);
    vector(writer, &value.normal);
    writer.f32(value.u);
    writer.f32(value.v);
}

fn lineage_vertex(writer: &mut BinaryWriter, value: &LineageVertex) {
    vector(writer, &value.position);
    vector(writer, &value.normal);
    writer.f32(value.u);
    writer.f32(value.v);
    writer.bytes(&value.bones);
    for &weight in &value.weights {
        writer.f32(weight);
    }
}

fn mesh_wedges(writer: &mut BinaryWriter, values: &[MeshWedge]) -> Result<(), String> {
    count(writer, values.len())?;
    for value in values {
        mesh_wedge(writer, value);
    }
    Ok(())
}

fn mesh_wedge(writer: &mut BinaryWriter, value: &MeshWedge) {
    writer.u16(value.vertex_index);
    writer.f32(value.u);
    writer.f32(value.v);
}

fn mesh_faces(writer: &mut BinaryWriter, values: &[MeshFace]) -> Result<(), String> {
    count(writer, values.len())?;
    for value in values {
        mesh_face(writer, value);
    }
    Ok(())
}

fn mesh_face(writer: &mut BinaryWriter, value: &MeshFace) {
    for &index in &value.wedge_indices {
        writer.u16(index);
    }
    writer.u16(value.material_index);
}

fn lazy<T>(
    writer: &mut BinaryWriter,
    values: &[T],
    context: &Context<'_>,
    mut item: impl FnMut(&mut BinaryWriter, &T),
) -> Result<(), String> {
    let end = writer.position();
    writer.i32(0);
    count(writer, values.len())?;
    for value in values {
        item(writer, value);
    }
    writer.patch_i32(end, absolute(writer, context)?);
    Ok(())
}

fn lazy_vectors(
    writer: &mut BinaryWriter,
    value: &LazyArray<Vector3>,
    context: &Context<'_>,
) -> Result<(), String> {
    lazy(writer, &value.values, context, vector)
}

fn lazy_wedges(
    writer: &mut BinaryWriter,
    value: &LazyArray<MeshWedge>,
    context: &Context<'_>,
) -> Result<(), String> {
    lazy(writer, &value.values, context, mesh_wedge)
}

fn lazy_faces(
    writer: &mut BinaryWriter,
    value: &LazyArray<MeshFace>,
    context: &Context<'_>,
) -> Result<(), String> {
    lazy(writer, &value.values, context, mesh_face)
}

fn lazy_triangles(
    writer: &mut BinaryWriter,
    value: &LazyArray<MeshTriangle>,
    context: &Context<'_>,
) -> Result<(), String> {
    lazy(writer, &value.values, context, |writer, value| {
        for &index in &value.wedge_indices {
            writer.u16(index);
        }
        writer.u8(value.material_index);
        writer.u8(value.auxiliary_material_index);
        writer.u32(value.smoothing_groups);
    })
}

fn lazy_influences(
    writer: &mut BinaryWriter,
    value: &LazyArray<VertexInfluence>,
    context: &Context<'_>,
) -> Result<(), String> {
    lazy(writer, &value.values, context, |writer, value| {
        writer.f32(value.weight);
        writer.u16(value.point_index);
        writer.u16(value.bone_index);
    })
}

fn lazy_u16(
    writer: &mut BinaryWriter,
    value: &LazyArray<u16>,
    context: &Context<'_>,
) -> Result<(), String> {
    lazy(writer, &value.values, context, |writer, value| {
        writer.u16(*value)
    })
}
