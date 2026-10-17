use super::*;
use crate::combiner::{CombinedMaterial, CombinedMaterials, combined_material};
use crate::scene::{
    PF_AUTO_U_PAN, PF_AUTO_V_PAN, masked_texture_image, material_cull_mode, opaque_texture_image,
    projector_transform, radar_atlas_image, repeating_texture_image, resolve_texture,
    self_illumination_image, standard_material, texture_frames, unreal_direction_vec,
};
use crate::terrain::{LoadedTerrainLayer, TerrainMaterial, layer_repeats, terrain_extension};
use bevy::light::NotShadowCaster;

pub(super) struct ViewerPresentationPlugin;

impl Plugin for ViewerPresentationPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, setup);
    }
}

#[derive(SystemParam)]
struct SceneAssets<'w> {
    meshes: ResMut<'w, Assets<Mesh>>,
    materials: ResMut<'w, Assets<StandardMaterial>>,
    images: ResMut<'w, Assets<Image>>,
    inverse_bindposes: ResMut<'w, Assets<SkinnedMeshInverseBindposes>>,
    combined: ResMut<'w, Assets<CombinedMaterial>>,
    combined_layers: ResMut<'w, CombinedMaterials>,
    terrain_materials: ResMut<'w, Assets<TerrainMaterial>>,
}

fn setup(
    mut commands: Commands,
    scene: Res<MapScene>,
    navigation: Res<MapNavigation>,
    assets: SceneAssets,
) {
    let SceneAssets {
        mut meshes,
        mut materials,
        mut images,
        mut inverse_bindposes,
        mut combined,
        mut combined_layers,
        mut terrain_materials,
    } = assets;
    commands.insert_resource(TeleportMenuState::default());
    commands.insert_resource(WorldMapState::default());
    commands.insert_resource(GameEventMenuState::default());
    commands.insert_resource(TimeOfDay::default());
    let game_events = GameEventEffects {
        counts: scene.game_event_counts(),
        playing: BTreeSet::new(),
    };
    let game_event_summary_text = game_event_summary(&game_events);
    let image_handles: HashMap<_, _> = scene
        .textures
        .iter()
        .map(|(name, texture)| (name.clone(), images.add(repeating_texture_image(texture))))
        .collect();
    // A shader masked by a second texture needs its own copy of the texture it
    // draws, with that mask baked into the alpha channel.
    let masked_images: HashMap<_, _> = scene
        .materials
        .keys()
        .filter_map(|name| {
            let resolved = resolve_texture(&scene, name)?;
            let texture = scene.textures.get(&resolved.texture)?;
            // A constant opacity is keyed by the texture alone.
            let (mask, image) = match resolved.opacity? {
                MaterialOpacity::Texture(mask) => {
                    let image = masked_texture_image(texture, scene.textures.get(&mask)?);
                    (mask, image)
                }
                MaterialOpacity::Constant(_) => (String::new(), opaque_texture_image(texture)),
            };
            Some(((resolved.texture.clone(), mask), images.add(image)))
        })
        .collect();
    // A shader whose self-illumination is masked glows only where its mask
    // allows, so it needs an emission texture of its own.
    let glow_images: HashMap<_, _> = scene
        .materials
        .keys()
        .filter_map(|name| {
            let resolved = resolve_texture(&scene, name)?;
            let mask = resolved
                .emissive_mask
                .clone()
                .filter(|_| resolved.emissive)?;
            let key = (resolved.texture.clone(), mask.clone());
            let texture = scene.textures.get(&resolved.texture)?;
            let mask = scene.textures.get(&mask)?;
            Some((key, images.add(self_illumination_image(texture, mask))))
        })
        .collect();
    let material_glow = |resolved: &ResolvedTexture| {
        let mask = resolved.emissive_mask.clone()?;
        glow_images.get(&(resolved.texture.clone(), mask)).cloned()
    };
    let material_image = |resolved: &ResolvedTexture| {
        let mask = match &resolved.opacity {
            Some(MaterialOpacity::Texture(mask)) => mask.clone(),
            Some(MaterialOpacity::Constant(_)) => String::new(),
            None => return image_handles.get(&resolved.texture),
        };
        masked_images
            .get(&(resolved.texture.clone(), mask))
            .or_else(|| image_handles.get(&resolved.texture))
    };
    // A combiner draws two moving layers at once, which the standard material
    // cannot, so those surfaces get a material of their own: plain, mirrored,
    // and the two-sided variant BSP surfaces use.
    let mut combined_handles = HashMap::new();
    for name in scene.materials.keys() {
        let Some(resolved) = resolve_texture(&scene, name) else {
            continue;
        };
        let Some(second) = resolved.second.clone() else {
            continue;
        };
        let (Some(texture), Some(second_image)) = (
            material_image(&resolved),
            image_handles.get(&second.texture),
        ) else {
            continue;
        };
        for (material_override, mirrored) in [
            (MaterialOverride::default(), false),
            (MaterialOverride::default(), true),
            (
                MaterialOverride {
                    two_sided: true,
                    ..default()
                },
                false,
            ),
        ] {
            let handle = combined.add(combined_material(
                standard_material(
                    &resolved,
                    texture.clone(),
                    material_glow(&resolved),
                    mirrored,
                    material_override,
                ),
                &second,
                second_image.clone(),
            ));
            combined_layers
                .0
                .push((handle.clone(), resolved.motion, second.motion));
            combined_handles.insert((name.clone(), material_override, mirrored), handle);
        }
    }
    let combined_for =
        |name: Option<&String>, material_override: MaterialOverride, mirrored: bool| {
            combined_handles
                .get(&(name?.to_ascii_lowercase(), material_override, mirrored))
                .cloned()
        };
    // An Unreal shader's specular layer is added over the surface whatever
    // its opacity. Lineage's waterfalls are mostly this: a faint tint with an
    // animated sheet of flowing water laid over it.
    // Unreal added these layers to gamma-encoded colour, where a faint sheet
    // still reads as bright water; added in linear light it all but vanishes.
    const SPECULAR_GAIN: f32 = 3.0;
    let mut specular_handles = HashMap::new();
    let mut animated = AnimatedMaterials::default();
    for name in scene.materials.keys() {
        let Some(layer) = resolve_texture(&scene, name).and_then(|resolved| resolved.specular)
        else {
            continue;
        };
        let (frames, rate) = texture_frames(&scene, &layer.texture);
        // A layer masked by its own texture already carries the mask in its
        // alpha, which additive drawing applies.
        let mask = layer
            .mask
            .filter(|mask| !frames.contains(mask))
            .and_then(|mask| scene.textures.get(&mask));
        let frames: Vec<_> = frames
            .iter()
            .filter_map(|frame| match mask {
                Some(mask) => {
                    Some(images.add(masked_texture_image(scene.textures.get(frame)?, mask)))
                }
                None => image_handles.get(frame).cloned(),
            })
            .collect();
        let Some(first) = frames.first().cloned() else {
            continue;
        };
        let material = materials.add(StandardMaterial {
            base_color: Color::LinearRgba(LinearRgba::gray(SPECULAR_GAIN)),
            base_color_texture: Some(first),
            alpha_mode: AlphaMode::Add,
            unlit: true,
            double_sided: true,
            cull_mode: None,
            ..default()
        });
        if frames.len() > 1 && rate > 0.0 {
            animated.0.push((material.clone(), frames, rate));
        }
        specular_handles.insert(name.clone(), material);
    }
    commands.insert_resource(animated);
    let specular_for =
        |name: Option<&String>| specular_handles.get(&name?.to_ascii_lowercase()).cloned();
    // Each terrain layer paints its texture over the ones before it wherever
    // its alpha map allows.
    let terrain_layers: Vec<_> = scene
        .terrain
        .iter()
        .flat_map(|terrain| {
            terrain.layers.layers.iter().filter_map(|layer| {
                let resolved = resolve_texture(&scene, &layer.texture)?;
                let texture = scene.textures.get(&resolved.texture)?;
                Some(LoadedTerrainLayer {
                    texture,
                    alpha_map: layer
                        .alpha_map
                        .as_ref()
                        .and_then(|map| scene.textures.get(&map.to_ascii_lowercase()))
                        .map(|map| &**map),
                    repeats: layer_repeats(terrain.layers.quad_size, layer, texture),
                })
            })
        })
        .collect();
    let terrain_size = scene.terrain.as_ref().map_or(Vec2::ONE, |terrain| {
        Vec2::new(terrain.width as f32, terrain.height as f32)
    });
    let terrain_material = terrain_materials.add(TerrainMaterial {
        base: StandardMaterial {
            base_color: Color::WHITE,
            perceptual_roughness: 0.95,
            // UE terrains are height-field sheets. Keeping both faces visible is
            // useful for stacked maps and prevents an upper sheet disappearing
            // completely when the free camera crosses beneath it.
            double_sided: true,
            cull_mode: None,
            ..default()
        },
        extension: terrain_extension(terrain_size, &terrain_layers, &mut images),
    });
    let selection_highlight = materials.add(StandardMaterial {
        base_color: Color::srgba(1.0, 0.72, 0.05, 0.65),
        emissive: LinearRgba::new(1.0, 0.25, 0.0, 1.0),
        alpha_mode: AlphaMode::Blend,
        double_sided: true,
        cull_mode: None,
        ..default()
    });
    commands.insert_resource(SelectionState {
        selected: None,
        highlight: selection_highlight,
        cursor: None,
        hits: Vec::new(),
        hit_index: 0,
    });
    if let Some(terrain) = &scene.terrain {
        let indices = reflected_winding(&terrain_indices(
            terrain.width,
            terrain.height,
            &terrain.quad_visibility,
            &terrain.edge_turns,
        ));
        let positions: Vec<_> = terrain
            .positions
            .iter()
            .map(|position| {
                ((Vec3::from_array(*position) - scene.origin) / UNREAL_UNITS_PER_METER).to_array()
            })
            .collect();
        let normals = smooth_normals(&positions, &indices);
        let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, selectable_mesh_usage());
        mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
        mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
        if terrain_vertex_colors_are_meaningful(&terrain.colors) {
            mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, terrain.colors.clone());
        }
        mesh.insert_attribute(
            Mesh::ATTRIBUTE_UV_0,
            (0..terrain.height)
                .flat_map(|y| {
                    // The terrain material scales grid positions itself.
                    (0..terrain.width).map(move |x| [x as f32, y as f32])
                })
                .collect::<Vec<_>>(),
        );
        mesh.insert_indices(Indices::U32(indices));
        commands.spawn((
            Name::new("Terrain"),
            Mesh3d(meshes.add(mesh)),
            MeshMaterial3d(terrain_material),
            SelectableObject {
                actor: "TerrainInfo".into(),
                object: "Engine.TerrainInfo".into(),
                material: terrain
                    .layers
                    .layers
                    .first()
                    .map(|layer| layer.texture.clone()),
                unreal_location: unreal_direction_vec(scene.origin),
                export: None,
            },
            RayCastBackfaces,
        ));
    }

    let fallback_material = materials.add(StandardMaterial {
        base_color: Color::srgb(0.62, 0.65, 0.72),
        perceptual_roughness: 0.8,
        cull_mode: material_cull_mode(false, false),
        ..default()
    });
    let mirrored_fallback_material = materials.add(StandardMaterial {
        base_color: Color::srgb(0.62, 0.65, 0.72),
        perceptual_roughness: 0.8,
        cull_mode: material_cull_mode(false, true),
        ..default()
    });
    let bsp_fallback_material = materials.add(StandardMaterial {
        base_color: Color::srgb(0.62, 0.65, 0.72),
        perceptual_roughness: 0.8,
        double_sided: true,
        cull_mode: None,
        ..default()
    });
    let material_handles: HashMap<_, _> = scene
        .materials
        .keys()
        .filter_map(|name| {
            let resolved = resolve_texture(&scene, name)?;
            let texture = material_image(&resolved)?;
            Some((
                name.clone(),
                materials.add(standard_material(
                    &resolved,
                    texture.clone(),
                    material_glow(&resolved),
                    false,
                    MaterialOverride::default(),
                )),
            ))
        })
        .collect();
    let mirrored_material_handles: HashMap<_, _> = scene
        .materials
        .keys()
        .filter_map(|name| {
            let resolved = resolve_texture(&scene, name)?;
            let texture = material_image(&resolved)?;
            Some((
                name.clone(),
                materials.add(standard_material(
                    &resolved,
                    texture.clone(),
                    material_glow(&resolved),
                    true,
                    MaterialOverride::default(),
                )),
            ))
        })
        .collect();
    let bsp_material_handles: HashMap<_, _> = scene
        .materials
        .keys()
        .filter_map(|name| {
            let resolved = resolve_texture(&scene, name)?;
            let texture = material_image(&resolved)?;
            Some((
                name.clone(),
                materials.add(standard_material(
                    &resolved,
                    texture.clone(),
                    material_glow(&resolved),
                    false,
                    MaterialOverride {
                        two_sided: true,
                        ..default()
                    },
                )),
            ))
        })
        .collect();
    let mut handles = HashMap::new();
    let mut override_material_handles: HashMap<
        (String, bool, MaterialOverride),
        Handle<StandardMaterial>,
    > = HashMap::new();
    let mut shared_handles = HashMap::new();
    for (name, geometry) in &scene.meshes {
        let geometry_id = Arc::as_ptr(geometry) as usize;
        let handle = shared_handles.entry(geometry_id).or_insert_with(|| {
            geometry
                .parts
                .iter()
                .map(|part| {
                    let mut mesh =
                        Mesh::new(PrimitiveTopology::TriangleList, selectable_mesh_usage());
                    let positions: Vec<_> = geometry
                        .positions
                        .iter()
                        .map(|position| {
                            (Vec3::from_array(*position) / UNREAL_UNITS_PER_METER).to_array()
                        })
                        .collect();
                    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
                    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, geometry.normals.clone());
                    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, geometry.uvs.clone());
                    // Only a combiner masked by the mesh's painted alpha reads
                    // it; any other material would take it as transparency.
                    if let Some(alpha) = &geometry.vertex_alpha
                        && part
                            .material
                            .as_ref()
                            .and_then(|name| resolve_texture(&scene, name))
                            .and_then(|resolved| resolved.second)
                            .is_some_and(|second| second.vertex_mask)
                    {
                        mesh.insert_attribute(
                            Mesh::ATTRIBUTE_COLOR,
                            alpha
                                .iter()
                                .map(|alpha| [1.0, 1.0, 1.0, *alpha])
                                .collect::<Vec<_>>(),
                        );
                    }
                    if let Some(skin) = &geometry.skin {
                        mesh.insert_attribute(
                            Mesh::ATTRIBUTE_JOINT_INDEX,
                            VertexAttributeValues::Uint16x4(skin.joint_indices.clone()),
                        );
                        mesh.insert_attribute(
                            Mesh::ATTRIBUTE_JOINT_WEIGHT,
                            skin.joint_weights.clone(),
                        );
                    }
                    mesh.insert_indices(Indices::U32(reflected_winding(&part.indices)));
                    (meshes.add(mesh), part.material.clone())
                })
                .collect::<Vec<_>>()
        });
        handles.insert(name.clone(), handle.clone());
    }
    for instance in &scene.instances {
        let Some(parts) = handles.get(&instance.mesh.to_ascii_lowercase()) else {
            continue;
        };
        let translation = (instance.location - scene.origin) / UNREAL_UNITS_PER_METER;
        let units_to_radians = std::f32::consts::TAU / 65_536.0;
        let rotation = unreal_rotation(instance.rotation * units_to_radians);
        let mirrored = instance.scale.x * instance.scale.y * instance.scale.z < 0.0;
        let geometry = scene.meshes.get(&instance.mesh.to_ascii_lowercase());
        let skinned_mesh = geometry.and_then(|geometry| {
            geometry.skin.as_ref().map(|skin| {
                spawn_skeleton(
                    &mut commands,
                    &mut inverse_bindposes,
                    skin,
                    scene.animations.get(&skin.animation).cloned(),
                    Transform {
                        translation,
                        rotation,
                        scale: instance.scale,
                    },
                )
            })
        });
        for (part_index, (mesh, material_name)) in parts.iter().enumerate() {
            let applicable_materials = if mirrored {
                &mirrored_material_handles
            } else {
                &material_handles
            };
            let material = material_name
                .as_ref()
                .and_then(|name| {
                    let name = name.to_ascii_lowercase();
                    if instance.material_override == MaterialOverride::default() {
                        return applicable_materials.get(&name).cloned();
                    }
                    let key = (name.clone(), mirrored, instance.material_override);
                    if let Some(material) = override_material_handles.get(&key) {
                        return Some(material.clone());
                    }
                    let resolved = resolve_texture(&scene, &name)?;
                    let texture = material_image(&resolved)?.clone();
                    let material = materials.add(standard_material(
                        &resolved,
                        texture,
                        material_glow(&resolved),
                        mirrored,
                        instance.material_override,
                    ));
                    override_material_handles.insert(key, material.clone());
                    Some(material)
                })
                .unwrap_or_else(|| {
                    if mirrored {
                        mirrored_fallback_material.clone()
                    } else {
                        fallback_material.clone()
                    }
                });
            let mut entity = commands.spawn((
                Name::new(format!("{} part {part_index}", instance.name)),
                Mesh3d(mesh.clone()),
                Transform {
                    translation,
                    rotation,
                    scale: instance.scale,
                },
                SelectableObject {
                    actor: instance.name.clone(),
                    object: instance.mesh.clone(),
                    material: material_name.clone(),
                    unreal_location: unreal_direction_vec(instance.location),
                    // Skinned parts follow their skeleton, so only rigid
                    // meshes are moved by the editor.
                    export: instance
                        .source
                        .filter(|_| skinned_mesh.is_none())
                        .map(|source| source.export),
                },
                RayCastBackfaces,
            ));
            match combined_for(material_name.as_ref(), instance.material_override, mirrored) {
                Some(combined) => entity.insert(MeshMaterial3d(combined)),
                None => entity.insert(MeshMaterial3d(material)),
            };
            if skinned_mesh.is_none()
                && let Some(specular) = specular_for(material_name.as_ref())
            {
                entity.with_child((
                    Mesh3d(mesh.clone()),
                    MeshMaterial3d(specular),
                    NotShadowCaster,
                ));
            }
            if let Some(skinned_mesh) = &skinned_mesh {
                entity
                    .insert(skinned_mesh.clone())
                    .insert(Transform::IDENTITY);
            }
        }
    }
    for (geometry_index, geometry) in scene.bsp.iter().enumerate() {
        for (part_index, part) in geometry.parts.iter().enumerate() {
            let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, selectable_mesh_usage());
            mesh.insert_attribute(
                Mesh::ATTRIBUTE_POSITION,
                geometry
                    .positions
                    .iter()
                    .map(|position| {
                        ((Vec3::from_array(*position) - scene.origin) / UNREAL_UNITS_PER_METER)
                            .to_array()
                    })
                    .collect::<Vec<_>>(),
            );
            mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, geometry.normals.clone());
            mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, geometry.uvs.clone());
            mesh.insert_indices(Indices::U32(reflected_winding(&part.indices)));
            let material = part
                .material
                .as_ref()
                .and_then(|name| bsp_material_handles.get(&name.to_ascii_lowercase()))
                .cloned()
                .unwrap_or_else(|| bsp_fallback_material.clone());
            let mesh = meshes.add(mesh);
            let mut entity = commands.spawn((
                Name::new(format!("BSP model {geometry_index} part {part_index}")),
                Mesh3d(mesh.clone()),
                SelectableObject {
                    actor: format!("BSP model {geometry_index}"),
                    object: "Engine.Model".into(),
                    material: part.material.clone(),
                    unreal_location: unreal_direction_vec(scene.origin),
                    export: None,
                },
                RayCastBackfaces,
            ));
            let two_sided = MaterialOverride {
                two_sided: true,
                ..default()
            };
            match combined_for(part.material.as_ref(), two_sided, false) {
                Some(combined) => entity.insert(MeshMaterial3d(combined)),
                None => entity.insert(MeshMaterial3d(material)),
            };
            if let Some(specular) = specular_for(part.material.as_ref()) {
                entity.with_child((Mesh3d(mesh), MeshMaterial3d(specular), NotShadowCaster));
            }
        }
    }

    particles::spawn_particle_systems(
        &mut commands,
        &scene,
        &mut meshes,
        &mut materials,
        &mut images,
    );
    let mut panning = PanningMaterials::default();
    commands.insert_resource(
        scene
            .sky
            .as_ref()
            .and_then(|sky| {
                let sun = sky.bodies.iter().find(|body| !body.night)?;
                Some(SunPath::through(sun.location - sky.origin))
            })
            .unwrap_or_default(),
    );
    if let Some(sky) = &scene.sky {
        commands.insert_resource(SkyRoom { bounds: sky.bounds });
        // The sky is drawn unlit by its own camera, so it keeps the brightness
        // it was painted with whatever the zone's ambient light says.
        let mut sky_materials: HashMap<String, Handle<StandardMaterial>> = HashMap::new();
        for geometry in &sky.geometry {
            for (part_index, part) in geometry.parts.iter().enumerate() {
                let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, selectable_mesh_usage());
                mesh.insert_attribute(
                    Mesh::ATTRIBUTE_POSITION,
                    geometry
                        .positions
                        .iter()
                        .map(|position| {
                            ((Vec3::from_array(*position) - sky.origin) / UNREAL_UNITS_PER_METER)
                                .to_array()
                        })
                        .collect::<Vec<_>>(),
                );
                mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, geometry.normals.clone());
                mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, geometry.uvs.clone());
                mesh.insert_indices(Indices::U32(reflected_winding(&part.indices)));
                let material = part.material.as_ref().and_then(|name| {
                    let name = name.to_ascii_lowercase();
                    if let Some(material) = sky_materials.get(&name) {
                        return Some(material.clone());
                    }
                    let resolved = resolve_texture(&scene, &name)?;
                    let texture = material_image(&resolved)?.clone();
                    let material = materials.add(StandardMaterial {
                        unlit: true,
                        ..standard_material(
                            &resolved,
                            texture,
                            material_glow(&resolved),
                            false,
                            MaterialOverride {
                                two_sided: true,
                                ..default()
                            },
                        )
                    });
                    // A sky surface drifts at its own panner's rate plus the
                    // zone's, and the zone only pans what the mapper flagged.
                    let motion = resolved.motion.then(UvMotion {
                        pan: Vec2::new(
                            if part.poly_flags & PF_AUTO_U_PAN != 0 {
                                sky.pan.x
                            } else {
                                0.0
                            },
                            if part.poly_flags & PF_AUTO_V_PAN != 0 {
                                sky.pan.y
                            } else {
                                0.0
                            },
                        ),
                        ..default()
                    });
                    if !motion.is_still() {
                        panning.0.push((material.clone(), motion));
                    }
                    sky_materials.insert(name, material.clone());
                    Some(material)
                });
                commands.spawn((
                    Name::new(format!("Sky part {part_index}")),
                    Mesh3d(meshes.add(mesh)),
                    MeshMaterial3d(material.unwrap_or_else(|| bsp_fallback_material.clone())),
                    RenderLayers::layer(1),
                ));
            }
        }
        // The sky camera never moves, so a body facing its origin once faces
        // it for good.
        let mut body_quad = Mesh::new(PrimitiveTopology::TriangleList, selectable_mesh_usage());
        body_quad.insert_attribute(
            Mesh::ATTRIBUTE_POSITION,
            vec![
                [-0.5, -0.5, 0.0],
                [0.5, -0.5, 0.0],
                [0.5, 0.5, 0.0],
                [-0.5, 0.5, 0.0],
            ],
        );
        body_quad.insert_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0, 0.0, 1.0]; 4]);
        body_quad.insert_attribute(
            Mesh::ATTRIBUTE_UV_0,
            vec![[0.0, 1.0], [1.0, 1.0], [1.0, 0.0], [0.0, 0.0]],
        );
        body_quad.insert_indices(Indices::U32(vec![0, 1, 2, 0, 2, 3]));
        let body_quad = meshes.add(body_quad);
        for body in &sky.bodies {
            let Some(resolved) = resolve_texture(&scene, &body.texture) else {
                continue;
            };
            let Some(texture) = material_image(&resolved) else {
                continue;
            };
            // Lineage's sun and moon sit almost on top of their zone actor
            // and are drawn at a radius that only makes sense once the zone's
            // own draw scale is applied. Each rides just inside the wall of
            // the sky room so it never pokes through the backdrop; this is
            // where the map left it, and the clock moves it from here.
            let direction = (body.location - sky.origin).normalize_or(Vec3::Y);
            let position = direction * sky_wall_distance(sky.bounds, direction) * 0.85
                / UNREAL_UNITS_PER_METER;
            commands.spawn((
                Name::new(body.name.clone()),
                Mesh3d(body_quad.clone()),
                MeshMaterial3d(materials.add(StandardMaterial {
                    base_color: Color::WHITE,
                    base_color_texture: Some(texture.clone()),
                    alpha_mode: AlphaMode::Add,
                    unlit: true,
                    double_sided: true,
                    cull_mode: None,
                    ..default()
                })),
                Transform::from_translation(position)
                    .looking_at(Vec3::ZERO, Vec3::Y)
                    .with_scale(Vec3::splat(
                        2.0 * body.radius * sky.scale / UNREAL_UNITS_PER_METER,
                    )),
                RenderLayers::layer(1),
                SkyBodyPhase { night: body.night },
            ));
        }
        commands.spawn((
            Name::new("Sky camera"),
            Camera3d::default(),
            Camera {
                order: -1,
                ..default()
            },
            RenderLayers::layer(1),
            Transform::default(),
            SkyCamera,
        ));
    }

    let moving = |name: &str| {
        resolve_texture(&scene, name)
            .map(|resolved| resolved.motion)
            .filter(|motion| !motion.is_still())
    };
    for (name, material) in material_handles
        .iter()
        .chain(&mirrored_material_handles)
        .chain(&bsp_material_handles)
    {
        if let Some(motion) = moving(name) {
            panning.0.push((material.clone(), motion));
        }
    }
    for ((name, _, _), material) in &override_material_handles {
        if let Some(motion) = moving(name) {
            panning.0.push((material.clone(), motion));
        }
    }
    commands.insert_resource(panning);
    for decal in &scene.decals {
        let Some(resolved) = resolve_texture(&scene, &decal.material) else {
            continue;
        };
        let Some(texture_geometry) = scene.textures.get(&resolved.texture) else {
            continue;
        };
        // UE2 projectors can be perspective projected-light masks. Bevy's
        // clustered decals are orthographic volumes and would paint those
        // opaque masks onto side/back-facing surfaces. Alpha-bearing decals
        // are safe to approximate with the clustered decal path.
        if !has_alpha_channel(texture_geometry) {
            continue;
        }
        let Some(texture) = image_handles.get(&resolved.texture) else {
            continue;
        };
        commands.spawn((
            Name::new(decal.name.clone()),
            ClusteredDecal {
                base_color_texture: Some(texture.clone()),
                ..default()
            },
            projector_transform(decal, scene.origin),
        ));
    }

    commands.insert_resource(GlobalAmbientLight {
        color: scene.environment.ambient_color,
        brightness: scene.environment.ambient_brightness,
        ..default()
    });
    commands.insert_resource(LightingControl {
        base_brightness: scene.environment.ambient_brightness,
        multiplier: 1.0,
        daylight: 1.0,
    });
    commands.insert_resource(ClearColor(
        scene
            .environment
            .fog
            .as_ref()
            .map_or(Color::srgb(0.025, 0.03, 0.045), |fog| fog.color),
    ));
    let units_to_radians = std::f32::consts::TAU / 65_536.0;
    let spotlight_axis = Quat::from_rotation_y(-std::f32::consts::FRAC_PI_2);
    for light in &scene.lights {
        let translation = (light.location - scene.origin) / UNREAL_UNITS_PER_METER;
        let rotation = unreal_rotation(light.rotation * units_to_radians) * spotlight_axis;
        match light.kind {
            SceneLightKind::Point => {
                commands.spawn((
                    Name::new(light.name.clone()),
                    PointLight {
                        color: light.color,
                        intensity: light.brightness * 100.0,
                        range: (light.radius / 8.0).max(2.0),
                        shadow_maps_enabled: false,
                        ..default()
                    },
                    Transform::from_translation(translation),
                ));
            }
            SceneLightKind::Spot => {
                let outer_angle = (light.cone * std::f32::consts::PI / 255.0).clamp(0.05, 1.5);
                commands.spawn((
                    Name::new(light.name.clone()),
                    SpotLight {
                        color: light.color,
                        intensity: light.brightness * 100.0,
                        range: (light.radius / 8.0).max(2.0),
                        inner_angle: outer_angle * 0.75,
                        outer_angle,
                        shadow_maps_enabled: false,
                        ..default()
                    },
                    Transform {
                        translation,
                        rotation,
                        ..default()
                    },
                ));
            }
            SceneLightKind::Sun => {
                commands.spawn((
                    Name::new(light.name.clone()),
                    DirectionalLight {
                        color: light.color,
                        illuminance: light.brightness * 750.0,
                        shadow_maps_enabled: true,
                        ..default()
                    },
                    Daylight {
                        illuminance: light.brightness * 750.0,
                    },
                    Transform::from_rotation(rotation),
                ));
            }
        }
    }
    let camera_height = (scene.view_radius * 0.55).max(45.0);
    let camera_distance = (scene.view_radius * 1.1).max(90.0);
    let overview_position = scene.view_center + Vec3::new(0.0, camera_height, camera_distance);
    let camera_position = scene
        .player_starts
        .iter()
        .find(|start| start.name == "PlayerStart0")
        .map(|start| (start.location - scene.origin) / UNREAL_UNITS_PER_METER)
        .unwrap_or(overview_position);
    let camera_pitch = -camera_height.atan2(camera_distance);
    let initial_move_speed = DEFAULT_MOVE_SPEED;
    commands.insert_resource(SettingsMenuState {
        open: false,
        fog: scene.environment.fog.clone(),
        fog_enabled: scene.environment.fog.is_some(),
        initial_move_speed,
    });
    let mut camera = commands.spawn((
        Name::new("Fly camera"),
        Camera3d::default(),
        Camera {
            clear_color: if scene.sky.is_some() {
                ClearColorConfig::None
            } else {
                ClearColorConfig::Default
            },
            ..default()
        },
        Transform::from_translation(camera_position).looking_at(scene.view_center, Vec3::Y),
        FlyCamera {
            yaw: 0.0,
            pitch: camera_pitch,
            speed: initial_move_speed,
        },
    ));
    if let Some(fog) = &scene.environment.fog {
        camera.insert(distance_fog(fog));
    }
    commands.spawn((
        Name::new("Unreal coordinates"),
        Text::new("X 0  Y 0  Z 0"),
        TextFont {
            font_size: FontSize::Px(18.0),
            ..default()
        },
        TextColor(Color::WHITE),
        Node {
            position_type: PositionType::Absolute,
            right: px(12),
            top: px(12),
            padding: UiRect::axes(px(10), px(6)),
            border_radius: BorderRadius::all(px(4)),
            ..default()
        },
        BackgroundColor(Color::srgba(0.02, 0.025, 0.035, 0.78)),
        CoordinateReadout,
    ));
    commands.spawn((
        Name::new("Selection inspector"),
        Text::new("Left click to inspect; click again to cycle; Shift+click cycles backward"),
        TextFont {
            font_size: FontSize::Px(16.0),
            ..default()
        },
        TextColor(Color::WHITE),
        Node {
            position_type: PositionType::Absolute,
            right: px(12),
            top: px(52),
            max_width: px(620),
            padding: UiRect::axes(px(10), px(7)),
            border_radius: BorderRadius::all(px(4)),
            ..default()
        },
        BackgroundColor(Color::srgba(0.02, 0.025, 0.035, 0.86)),
        SelectionReadout,
    ));
    let panel = commands
        .spawn((
            Name::new("Player start teleport panel"),
            Node {
                position_type: PositionType::Absolute,
                left: percent(20),
                right: percent(20),
                top: percent(10),
                bottom: percent(10),
                flex_direction: FlexDirection::Column,
                padding: UiRect::all(px(16)),
                row_gap: px(8),
                border_radius: BorderRadius::all(px(8)),
                ..default()
            },
            BackgroundColor(Color::srgba(0.015, 0.02, 0.03, 0.96)),
            Visibility::Hidden,
            TeleportPanel,
            OverlayPanelKind::Teleport,
        ))
        .id();
    commands.entity(panel).with_children(|parent| {
        parent.spawn((
            Text::new("Player starts — click to teleport (T to close)"),
            TextFont {
                font_size: FontSize::Px(22.0),
                ..default()
            },
            TextColor(Color::WHITE),
        ));
        parent
            .spawn((
                Node {
                    width: percent(100),
                    flex_grow: 1.0,
                    flex_direction: FlexDirection::Column,
                    overflow: Overflow::scroll_y(),
                    scrollbar_width: 12.0,
                    row_gap: px(4),
                    ..default()
                },
                ScrollPosition::default(),
                TeleportScroll,
            ))
            .with_children(|list| {
                for start in &scene.player_starts {
                    list.spawn((
                        Button,
                        Node {
                            width: percent(100),
                            min_height: px(38),
                            padding: UiRect::axes(px(12), px(7)),
                            ..default()
                        },
                        BackgroundColor(Color::srgb(0.10, 0.13, 0.18)),
                        TeleportButton {
                            location: start.location,
                        },
                    ))
                    .with_children(|button| {
                        button.spawn((
                            Text::new(format!(
                                "{}    X {:.0}  Y {:.0}  Z {:.0}",
                                start.name, start.location.x, -start.location.z, start.location.y
                            )),
                            TextFont {
                                font_size: FontSize::Px(17.0),
                                ..default()
                            },
                            TextColor(Color::WHITE),
                        ));
                    });
                }
            });
    });
    let world_map_panel = commands
        .spawn((
            Name::new("World map panel"),
            Node {
                position_type: PositionType::Absolute,
                left: percent(4),
                right: percent(4),
                top: percent(4),
                bottom: percent(4),
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                row_gap: px(8),
                padding: UiRect::all(px(12)),
                border_radius: BorderRadius::all(px(8)),
                ..default()
            },
            BackgroundColor(Color::srgba(0.015, 0.02, 0.03, 0.97)),
            Visibility::Hidden,
            WorldMapPanel,
            OverlayPanelKind::WorldMap,
        ))
        .id();
    let radar_image = navigation
        .radar
        .as_ref()
        .map(|radar| images.add(radar_atlas_image(radar)));
    commands.entity(world_map_panel).with_children(|parent| {
        parent.spawn((
            Text::new("World map - click a region to load it (M to close)"),
            TextFont {
                font_size: FontSize::Px(22.0),
                ..default()
            },
            TextColor(Color::WHITE),
        ));
        if let Some(radar_image) = radar_image {
            parent.spawn((
                Button,
                ImageNode::new(radar_image),
                Node {
                    width: percent(100),
                    max_height: percent(88),
                    aspect_ratio: Some(RADAR_COLUMNS as f32 / RADAR_ROWS as f32),
                    ..default()
                },
                RelativeCursorPosition::default(),
                WorldMapImage,
            ));
        } else {
            parent.spawn((
                Text::new("L2_RadarMap.utx was not found or could not be decoded"),
                TextFont {
                    font_size: FontSize::Px(18.0),
                    ..default()
                },
                TextColor(Color::srgb(1.0, 0.55, 0.45)),
            ));
        }
        parent.spawn((
            Text::new("Move over the map to select a region"),
            TextFont {
                font_size: FontSize::Px(18.0),
                ..default()
            },
            TextColor(Color::WHITE),
            WorldMapHoverText,
        ));
    });
    let game_event_panel = commands
        .spawn((
            Name::new("Game event effects panel"),
            Node {
                position_type: PositionType::Absolute,
                left: percent(20),
                right: percent(20),
                top: percent(10),
                bottom: percent(10),
                flex_direction: FlexDirection::Column,
                padding: UiRect::all(px(16)),
                row_gap: px(8),
                border_radius: BorderRadius::all(px(8)),
                ..default()
            },
            BackgroundColor(Color::srgba(0.015, 0.02, 0.03, 0.96)),
            Visibility::Hidden,
            GameEventPanel,
            OverlayPanelKind::GameEvents,
        ))
        .id();
    commands.entity(game_event_panel).with_children(|parent| {
        parent.spawn((
            Text::new(if game_events.counts.is_empty() {
                "Game event effects — this map has none (G to close)".into()
            } else {
                format!(
                    "Game event effects — click an event to play it ({} component(s), G to close)",
                    game_events.components()
                )
            }),
            TextFont {
                font_size: FontSize::Px(22.0),
                ..default()
            },
            TextColor(Color::WHITE),
        ));
        parent
            .spawn((
                Node {
                    width: percent(100),
                    flex_grow: 1.0,
                    flex_direction: FlexDirection::Column,
                    overflow: Overflow::scroll_y(),
                    scrollbar_width: 12.0,
                    row_gap: px(4),
                    ..default()
                },
                ScrollPosition::default(),
                GameEventScroll,
            ))
            .with_children(|list| {
                for (event, count) in &game_events.counts {
                    list.spawn((
                        Button,
                        Node {
                            width: percent(100),
                            min_height: px(38),
                            padding: UiRect::axes(px(12), px(7)),
                            ..default()
                        },
                        BackgroundColor(Color::srgb(0.10, 0.13, 0.18)),
                        GameEventRow(*event),
                    ))
                    .with_children(|button| {
                        button.spawn((
                            Text::new(game_event_row_label(*event, *count, false)),
                            TextFont {
                                font_size: FontSize::Px(17.0),
                                ..default()
                            },
                            TextColor(Color::WHITE),
                            GameEventRowText(*event),
                        ));
                    });
                }
            });
    });
    commands.insert_resource(game_events);
    let settings_panel = commands
        .spawn((
            Name::new("Viewer settings panel"),
            Node {
                position_type: PositionType::Absolute,
                left: percent(32),
                right: percent(32),
                top: percent(22),
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Stretch,
                row_gap: px(12),
                padding: UiRect::all(px(20)),
                border_radius: BorderRadius::all(px(8)),
                ..default()
            },
            BackgroundColor(Color::srgba(0.015, 0.02, 0.03, 0.97)),
            Visibility::Hidden,
            SettingsPanel,
            OverlayPanelKind::Settings,
        ))
        .id();
    commands.entity(settings_panel).with_children(|parent| {
        parent.spawn((
            Text::new("Viewer settings (Esc to close)"),
            TextFont {
                font_size: FontSize::Px(24.0),
                ..default()
            },
            TextColor(Color::WHITE),
        ));
        parent
            .spawn((
                Button,
                Node {
                    width: percent(100),
                    min_height: px(42),
                    padding: UiRect::axes(px(12), px(8)),
                    ..default()
                },
                BackgroundColor(Color::srgb(0.10, 0.13, 0.18)),
                SettingsAction::ToggleFog,
            ))
            .with_children(|button| {
                button.spawn((
                    Text::new(if scene.environment.fog.is_some() {
                        "Fog: enabled (click to disable)"
                    } else {
                        "Fog: unavailable on this map"
                    }),
                    TextFont {
                        font_size: FontSize::Px(18.0),
                        ..default()
                    },
                    TextColor(Color::WHITE),
                    SettingsLabel::Fog,
                ));
            });
        parent
            .spawn((
                Button,
                Node {
                    width: percent(100),
                    min_height: px(42),
                    padding: UiRect::axes(px(12), px(8)),
                    ..default()
                },
                BackgroundColor(Color::srgb(0.10, 0.13, 0.18)),
                SettingsAction::ToggleDayNight,
            ))
            .with_children(|button| {
                button.spawn((
                    Text::new(time_of_day_summary(&TimeOfDay::default())),
                    TextFont {
                        font_size: FontSize::Px(18.0),
                        ..default()
                    },
                    TextColor(Color::WHITE),
                    SettingsLabel::DayNight,
                ));
            });
        parent
            .spawn(Node {
                width: percent(100),
                flex_direction: FlexDirection::Row,
                column_gap: px(8),
                ..default()
            })
            .with_children(|row| {
                for (label, action) in [
                    ("Shorter day", SettingsAction::ShorterDay),
                    ("Longer day", SettingsAction::LongerDay),
                ] {
                    row.spawn((
                        Button,
                        Node {
                            flex_grow: 1.0,
                            min_height: px(40),
                            padding: UiRect::axes(px(12), px(8)),
                            justify_content: JustifyContent::Center,
                            ..default()
                        },
                        BackgroundColor(Color::srgb(0.10, 0.13, 0.18)),
                        action,
                    ))
                    .with_children(|button| {
                        button.spawn((
                            Text::new(label),
                            TextFont {
                                font_size: FontSize::Px(17.0),
                                ..default()
                            },
                            TextColor(Color::WHITE),
                        ));
                    });
                }
            });
        parent
            .spawn((
                Button,
                Node {
                    width: percent(100),
                    min_height: px(42),
                    padding: UiRect::axes(px(12), px(8)),
                    ..default()
                },
                BackgroundColor(Color::srgb(0.10, 0.13, 0.18)),
                SettingsAction::ToggleGameEventEffects,
            ))
            .with_children(|button| {
                button.spawn((
                    Text::new(game_event_summary_text),
                    TextFont {
                        font_size: FontSize::Px(18.0),
                        ..default()
                    },
                    TextColor(Color::WHITE),
                    SettingsLabel::GameEvents,
                ));
            });
        parent.spawn((
            Text::new(format!("Movement speed: {initial_move_speed:.1} m/s")),
            TextFont {
                font_size: FontSize::Px(18.0),
                ..default()
            },
            TextColor(Color::WHITE),
            SettingsLabel::Speed,
        ));
        parent
            .spawn(Node {
                width: percent(100),
                flex_direction: FlexDirection::Row,
                column_gap: px(8),
                ..default()
            })
            .with_children(|row| {
                for (label, action) in [
                    ("Slower", SettingsAction::Slower),
                    ("Faster", SettingsAction::Faster),
                    ("Reset", SettingsAction::ResetSpeed),
                ] {
                    row.spawn((
                        Button,
                        Node {
                            flex_grow: 1.0,
                            min_height: px(40),
                            padding: UiRect::axes(px(12), px(8)),
                            justify_content: JustifyContent::Center,
                            ..default()
                        },
                        BackgroundColor(Color::srgb(0.10, 0.13, 0.18)),
                        action,
                    ))
                    .with_children(|button| {
                        button.spawn((
                            Text::new(label),
                            TextFont {
                                font_size: FontSize::Px(17.0),
                                ..default()
                            },
                            TextColor(Color::WHITE),
                        ));
                    });
                }
            });
        parent.spawn((
            Text::new("WASD move, Q/E descend/ascend, hold Shift for 4x speed; T player starts, M world map, G game events"),
            TextFont {
                font_size: FontSize::Px(15.0),
                ..default()
            },
            TextColor(Color::srgb(0.72, 0.76, 0.84)),
        ));
    });
}

fn spawn_skeleton(
    commands: &mut Commands,
    inverse_bindposes: &mut Assets<SkinnedMeshInverseBindposes>,
    skin: &SkinGeometry,
    animation: Option<Arc<AnimationGeometry>>,
    actor_transform: Transform,
) -> SkinnedMesh {
    let mut global_bindposes = vec![Mat4::IDENTITY; skin.bones.len()];
    for (index, bone) in skin.bones.iter().enumerate() {
        let local = bone.transform.to_matrix();
        global_bindposes[index] = bone
            .parent
            .and_then(|parent| global_bindposes.get(parent).copied())
            .map_or(local, |parent| parent * local);
    }
    let inverse_bindposes = inverse_bindposes.add(
        global_bindposes
            .into_iter()
            .map(|matrix| matrix.inverse())
            .collect::<Vec<_>>(),
    );
    let root = commands
        .spawn((Name::new("Skeletal mesh root"), actor_transform))
        .id();
    let joints = skin
        .bones
        .iter()
        .map(|bone| {
            commands
                .spawn((Name::new(bone.name.clone()), bone.transform))
                .id()
        })
        .collect::<Vec<_>>();
    for (index, bone) in skin.bones.iter().enumerate() {
        let parent = bone
            .parent
            .and_then(|parent| joints.get(parent))
            .unwrap_or(&root);
        commands.entity(*parent).add_child(joints[index]);
    }
    if let Some(animation) = animation {
        commands.entity(root).insert(AnimatedSkeleton {
            animation,
            joints: joints.clone(),
        });
    }
    SkinnedMesh {
        inverse_bindposes,
        joints,
    }
}

pub(super) fn game_event_summary(events: &GameEventEffects) -> String {
    if events.counts.is_empty() {
        return "Game event effects: none on this map".into();
    }
    format!(
        "Game event effects: {} of {} component(s) playing (click to toggle all, G to choose)",
        events.playing_components(),
        events.components()
    )
}

pub(super) fn game_event_row_label(event: i32, components: usize, playing: bool) -> String {
    format!(
        "Event {event}    {components} component(s)    {}",
        if playing { "playing" } else { "dormant" }
    )
}

/// How far a direction travels from the sky zone before it leaves the room.
pub(super) fn sky_wall_distance(bounds: (Vec3, Vec3), direction: Vec3) -> f32 {
    (0..3)
        .filter_map(|axis| {
            let step = direction[axis];
            if step.abs() < f32::EPSILON {
                return None;
            }
            let wall = if step > 0.0 { bounds.1 } else { bounds.0 };
            Some(wall[axis] / step)
        })
        .filter(|distance| *distance > 0.0)
        .fold(f32::INFINITY, f32::min)
}

pub(super) fn time_of_day_summary(clock: &TimeOfDay) -> String {
    let minutes = (clock.hour * 60.0) as u32;
    let day = clock.day_length / 60.0;
    let day = if day >= 60.0 {
        format!("{:.0} h", day / 60.0)
    } else {
        format!("{day:.0} min")
    };
    format!(
        "Time of day: {:02}:{:02}, a day lasts {day} (click to skip 12 h)",
        minutes / 60 % 24,
        minutes % 60
    )
}
