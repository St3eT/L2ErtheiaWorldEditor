use super::*;

impl MapScene {
    pub(super) fn from_exports(
        exports: Vec<ExportDump>,
        object_paths: HashMap<i32, String>,
    ) -> Result<Self, String> {
        let mut terrain = None;
        let mut pending_terrain = None;
        let mut meshes = HashMap::new();
        let mut animations = HashMap::new();
        let mut materials = HashMap::new();
        let mut textures = HashMap::new();
        let mut actors = Vec::new();
        let mut lights = Vec::new();
        let mut particle_definitions = HashMap::new();
        let mut emitter_actors = Vec::new();
        let mut sky_zone = None;
        let mut sky_bodies = Vec::new();
        let mut decals = Vec::new();
        let mut player_starts = Vec::new();
        let mut environment = SceneEnvironment::default();
        let actor_brushes: BTreeSet<String> = exports
            .iter()
            .filter_map(|export| property(&export.properties, "Brush"))
            .map(|property| property.value.to_ascii_lowercase())
            .collect();
        let mut bsp = Vec::new();
        for export in exports {
            let material = material_geometry("", &export);
            if export.class == "Engine.PlayerStart"
                && let Some(location) = property(&export.properties, "Location")
                    .and_then(|property| triplet(&property.value))
            {
                player_starts.push(PlayerStart {
                    name: export.name.clone(),
                    location: unreal_direction_vec(location),
                });
            }
            match export.native_object {
                Some(NativeObject::TerrainInfo(value)) => {
                    let layers = terrain_layers(&export.properties);
                    let quad_visibility =
                        terrain_bitmap(&export.properties, "QuadVisibilityBitmap")?;
                    let edge_turns = terrain_bitmap(&export.properties, "EdgeTurnBitmap")?;
                    let topology = TerrainTopology {
                        quad_visibility,
                        edge_turns,
                    };
                    if !value.vertices.is_empty() {
                        terrain = Some(terrain_geometry(
                            value.heightmap_x,
                            value.heightmap_y,
                            value.vertices,
                            value.vertex_colors,
                            layers,
                            topology,
                        )?);
                    } else if let Some(texture) = property(&export.properties, "TerrainMap") {
                        let width = usize::try_from(value.heightmap_x)
                            .map_err(|_| "terrain width is negative")?;
                        let height = usize::try_from(value.heightmap_y)
                            .map_err(|_| "terrain height is negative")?;
                        pending_terrain = Some(PendingTerrain {
                            heightmap_texture: texture.value.to_ascii_lowercase(),
                            width,
                            height,
                            to_world: value.to_world,
                            colors: value.vertex_colors,
                            layers,
                            topology,
                        });
                    }
                }
                Some(NativeObject::StaticMesh(value)) => {
                    let materials = mesh_materials(&export.properties);
                    meshes.insert(
                        export.full_name.to_ascii_lowercase(),
                        Arc::new(mesh_geometry(value, materials)),
                    );
                }
                Some(NativeObject::SkeletalMesh(value)) => {
                    meshes.insert(
                        export.full_name.to_ascii_lowercase(),
                        Arc::new(skeletal_mesh_geometry(*value)),
                    );
                }
                Some(NativeObject::MeshAnimation(value)) => {
                    animations.insert(
                        export.full_name.to_ascii_lowercase(),
                        Arc::new(animation_geometry(value)),
                    );
                }
                Some(NativeObject::Texture(value)) => {
                    if let Some(texture) = texture_geometry(&export.properties, value) {
                        textures.insert(export.full_name.to_ascii_lowercase(), Arc::new(texture));
                    }
                }
                Some(NativeObject::Model(value)) => {
                    // Actor-owned brush models are local collision/CSG volumes. The
                    // unowned model contains the level's already-composed BSP.
                    if !actor_brushes.contains(&export.full_name.to_ascii_lowercase())
                        && let Ok(geometry) = bsp_geometry(&value.serialized_bsp, &object_paths)
                    {
                        bsp.push(geometry);
                    }
                }
                _ if matches!(
                    export.class.as_str(),
                    "Engine.ZoneInfo" | "Engine.SkyZoneInfo"
                ) =>
                {
                    if export.class == "Engine.SkyZoneInfo" {
                        sky_zone = sky_zone.or_else(|| sky_zone_view(&export));
                    }
                    merge_scene_environment(&mut environment, &export.properties);
                }
                _ if matches!(export.class.as_str(), "Engine.NSun" | "Engine.NMoon") => {
                    if let Some(body) = sky_body(&export) {
                        sky_bodies.push(body);
                    }
                }
                _ if export.class.contains("Projector") || export.class.contains("Decal") => {
                    if let Some(decal) = scene_decal(&export) {
                        decals.push(decal);
                    }
                }
                _ if matches!(
                    export.class.as_str(),
                    "Engine.StaticMeshActor" | "Engine.Mover"
                ) || property(&export.properties, "Mesh").is_some()
                    || property(&export.properties, "SkeletalMesh").is_some() =>
                {
                    if let Some(instance) = mesh_instance(&export) {
                        actors.push(instance);
                    }
                }
                _ if is_scene_light_class(&export.class) => lights.push(scene_light(&export)),
                _ if matches!(
                    export.class.as_str(),
                    "Engine.SpriteEmitter"
                        | "Engine.MeshEmitter"
                        | "Engine.VertMeshEmitter"
                        | "Engine.BeamEmitter"
                ) =>
                {
                    particle_definitions.insert(
                        export.name.to_ascii_lowercase(),
                        Arc::new(particles::particle_definition(&export)),
                    );
                }
                _ if export.class == "Engine.Emitter" => {
                    emitter_actors.push(emitter_actor(&export));
                }
                _ => {}
            }
            if let Some(material) = material {
                materials.insert(export.full_name.to_ascii_lowercase(), material);
            }
        }
        let sky = sky_zone.map(|zone| split_sky_geometry(&mut bsp, zone, sky_bodies));
        let (world_center, world_radius) = scene_extent(&terrain, &actors);
        let origin = terrain
            .as_ref()
            .and_then(|value| value.positions.first().copied())
            .map(Vec3::from_array)
            .unwrap_or(world_center);
        let view_center = (world_center - origin) / UNREAL_UNITS_PER_METER;
        let view_radius = (world_radius / UNREAL_UNITS_PER_METER).max(32.0);
        let embedded_meshes = meshes.len();

        let effects = emitter_actors
            .into_iter()
            .flat_map(|(name, location, visible_limit, game_event, references)| {
                references.into_iter().filter_map({
                    let definitions = &particle_definitions;
                    move |reference| {
                        Some(ParticleEffect {
                            name: format!("{name}.{reference}"),
                            location,
                            visible_limit,
                            game_event,
                            definition: definitions.get(&reference.to_ascii_lowercase())?.clone(),
                        })
                    }
                })
            })
            .collect();
        player_starts.sort_by_key(|start| player_start_number(&start.name));
        Ok(Self {
            origin,
            view_center,
            view_radius,
            terrain,
            pending_terrain,
            meshes,
            animations,
            materials,
            textures,
            instances: actors,
            lights,
            effects,
            decals,
            player_starts,
            bsp,
            environment,
            embedded_meshes,
            sky,
        })
    }

    pub(super) fn load_external_meshes(
        &mut self,
        asset_roots: &[PathBuf],
    ) -> Result<ExternalLoadReport, String> {
        let required_packages: BTreeSet<_> = self
            .instances
            .iter()
            .filter(|actor| !self.has_mesh(&actor.mesh))
            .filter_map(|actor| external_package_name(&actor.mesh))
            .collect();
        let package_files = index_asset_packages(asset_roots)?;
        let mut report = ExternalLoadReport::default();
        for package_name in required_packages {
            let Some(path) = package_files.get(&package_name) else {
                report.warnings.push(format!(
                    "external package {package_name}.usx was not found under {}",
                    display_roots(asset_roots)
                ));
                continue;
            };
            let package = match Package::read(path) {
                Ok(package) => package,
                Err(error) => {
                    report
                        .warnings
                        .push(format!("cannot load {}: {error}", path.display()));
                    continue;
                }
            };
            let mut decoded = 0;
            for export in package.dump().exports {
                let aliases = external_mesh_aliases(&package_name, &export.full_name);
                let definition = material_geometry(&package_name, &export);
                let geometry = match export.native_object {
                    Some(NativeObject::StaticMesh(mesh)) => mesh_geometry(
                        mesh,
                        qualify_material_paths(&package_name, mesh_materials(&export.properties)),
                    ),
                    Some(NativeObject::SkeletalMesh(mesh)) => {
                        let mut geometry = skeletal_mesh_geometry(*mesh);
                        for part in &mut geometry.parts {
                            if let Some(material) = part.material.take() {
                                part.material =
                                    Some(qualify_material_path(&package_name, material));
                            }
                        }
                        geometry
                    }
                    Some(NativeObject::MeshAnimation(animation)) => {
                        let animation = Arc::new(animation_geometry(animation));
                        for alias in aliases {
                            self.animations
                                .entry(alias)
                                .or_insert_with(|| animation.clone());
                        }
                        continue;
                    }
                    Some(NativeObject::Texture(texture)) => {
                        if let Some(decoded_texture) = texture_geometry(&export.properties, texture)
                        {
                            let decoded_texture = Arc::new(decoded_texture);
                            for alias in &aliases {
                                self.textures
                                    .entry(alias.clone())
                                    .or_insert_with(|| decoded_texture.clone());
                            }
                            report.loaded_textures += 1;
                        }
                        if let Some(definition) = definition {
                            for alias in aliases {
                                self.materials
                                    .entry(alias)
                                    .or_insert_with(|| definition.clone());
                            }
                        }
                        continue;
                    }
                    _ => continue,
                };
                let geometry = Arc::new(geometry);
                for alias in aliases {
                    self.meshes.entry(alias).or_insert_with(|| geometry.clone());
                }
                decoded += 1;
            }
            if decoded == 0 {
                report.warnings.push(format!(
                    "{} contains no decoded Engine.StaticMesh exports",
                    path.display()
                ));
            }
            report.loaded_packages += 1;
        }
        self.load_heightmap_terrain(&package_files, &mut report);
        self.load_materials(&package_files, &mut report);
        self.refresh_scene_extent();
        Ok(report)
    }

    fn load_heightmap_terrain(
        &mut self,
        package_files: &BTreeMap<String, PathBuf>,
        report: &mut ExternalLoadReport,
    ) {
        let Some(source) = self.pending_terrain.take() else {
            return;
        };
        let Some(package_name) = external_package_name(&source.heightmap_texture) else {
            report.warnings.push(format!(
                "terrain height map {} has no package name",
                source.heightmap_texture
            ));
            return;
        };
        let Some(path) = package_files.get(&package_name) else {
            report.warnings.push(format!(
                "terrain height-map package {package_name}.utx was not found"
            ));
            return;
        };
        let package = match Package::read(path) {
            Ok(package) => package,
            Err(error) => {
                report.warnings.push(format!(
                    "cannot load terrain height-map package {}: {error}",
                    path.display()
                ));
                return;
            }
        };
        let Some(export) = package.dump().exports.into_iter().find(|export| {
            qualify_object_path(&package_name, &export.full_name) == source.heightmap_texture
        }) else {
            report.warnings.push(format!(
                "terrain height map {} was not found in {}",
                source.heightmap_texture,
                path.display()
            ));
            return;
        };
        let format = property(&export.properties, "Format")
            .and_then(|property| property.value.parse::<u8>().ok());
        let Some(NativeObject::Texture(texture)) = export.native_object else {
            report.warnings.push(format!(
                "terrain height map {} is not a texture",
                source.heightmap_texture
            ));
            return;
        };
        let Some(mip) = texture.mipmaps.into_iter().next() else {
            report.warnings.push(format!(
                "terrain height map {} has no mipmaps",
                source.heightmap_texture
            ));
            return;
        };
        if format != Some(10) {
            report.warnings.push(format!(
                "terrain height map {} uses unsupported texture format {}",
                source.heightmap_texture,
                format.map_or_else(|| "unknown".into(), |value| value.to_string())
            ));
            return;
        }
        match terrain_from_g16_heightmap(
            source.width,
            source.height,
            &mip.data,
            &source.to_world,
            source.colors,
            source.layers,
            source.topology,
        ) {
            Ok(terrain) => {
                self.terrain = Some(terrain);
                report.loaded_material_packages += 1;
                report.loaded_textures += 1;
            }
            Err(error) => report.warnings.push(format!(
                "cannot decode terrain height map {}: {error}",
                source.heightmap_texture
            )),
        }
    }

    fn refresh_scene_extent(&mut self) {
        let (world_center, world_radius) = scene_extent(&self.terrain, &self.instances);
        self.origin = self
            .terrain
            .as_ref()
            .and_then(|terrain| terrain.positions.first().copied())
            .map(Vec3::from_array)
            .unwrap_or(world_center);
        self.view_center = (world_center - self.origin) / UNREAL_UNITS_PER_METER;
        self.view_radius = (world_radius / UNREAL_UNITS_PER_METER).max(32.0);
    }

    fn load_materials(
        &mut self,
        package_files: &BTreeMap<String, PathBuf>,
        report: &mut ExternalLoadReport,
    ) {
        let mut pending: BTreeSet<String> = self
            .meshes
            .values()
            .flat_map(|mesh| mesh.parts.iter())
            .filter_map(|part| part.material.as_deref())
            .map(str::to_ascii_lowercase)
            .collect();
        pending.extend(
            self.materials
                .values()
                .filter_map(|material| material.opacity.as_deref())
                .map(str::to_ascii_lowercase),
        );
        pending.extend(
            self.sky
                .iter()
                .flat_map(|sky| &sky.bodies)
                .map(|body| body.texture.to_ascii_lowercase()),
        );
        pending.extend(
            self.effects
                .iter()
                .filter_map(|effect| effect.definition.texture.as_deref())
                .map(str::to_ascii_lowercase),
        );
        pending.extend(
            self.decals
                .iter()
                .map(|decal| decal.material.to_ascii_lowercase()),
        );
        pending.extend(
            self.bsp
                .iter()
                .chain(self.sky.iter().flat_map(|sky| &sky.geometry))
                .flat_map(|geometry| geometry.parts.iter())
                .filter_map(|part| part.material.as_deref())
                .map(str::to_ascii_lowercase),
        );
        pending.extend(
            self.terrain
                .iter()
                .flat_map(|terrain| &terrain.layers.layers)
                .flat_map(|layer| [Some(&layer.texture), layer.alpha_map.as_ref()])
                .flatten()
                .map(|name| name.to_ascii_lowercase()),
        );
        let mut packages: HashMap<String, Vec<ExportDump>> = HashMap::new();
        let mut missing_packages = BTreeSet::new();
        let mut redirects = Vec::new();
        while let Some(material_path) = pending.pop_first() {
            if self.materials.contains_key(&material_path)
                || self.textures.contains_key(&material_path)
            {
                continue;
            }
            let Some(package_name) = external_package_name(&material_path) else {
                continue;
            };
            if missing_packages.contains(&package_name) {
                continue;
            }
            if !packages.contains_key(&package_name) {
                let Some(path) = package_files.get(&package_name) else {
                    missing_packages.insert(package_name.clone());
                    report
                        .warnings
                        .push(format!("material package {package_name}.utx was not found"));
                    continue;
                };
                match Package::read(path) {
                    Ok(package) => {
                        packages.insert(package_name.clone(), package.dump().exports);
                        report.loaded_material_packages += 1;
                    }
                    Err(error) => {
                        missing_packages.insert(package_name.clone());
                        report.warnings.push(format!(
                            "cannot load material package {}: {error}",
                            path.display()
                        ));
                        continue;
                    }
                }
            }
            let exports = packages.get_mut(&package_name).unwrap();
            let Some(index) = exports.iter().position(|export| {
                qualify_object_path(&package_name, &export.full_name) == material_path
            }) else {
                if let Some(target) = other_package_reading(&material_path, package_files) {
                    pending.insert(target.clone());
                    redirects.push((material_path, target));
                }
                continue;
            };
            let export = exports.swap_remove(index);
            let aliases = external_mesh_aliases(&package_name, &export.full_name);
            let definition = material_geometry(&package_name, &export);
            let palette = texture_palette(&package_name, &export.properties, exports);
            if let Some(NativeObject::Texture(texture)) = export.native_object
                && let Some(decoded) = match palette {
                    Some(palette) => paletted_texture_geometry(texture, &palette),
                    None => texture_geometry(&export.properties, texture),
                }
            {
                let decoded = Arc::new(decoded);
                for alias in &aliases {
                    self.textures
                        .entry(alias.clone())
                        .or_insert_with(|| decoded.clone());
                }
                report.loaded_textures += 1;
            }
            if let Some(definition) = definition {
                // A shader's opacity material is part of how the surface is
                // drawn, so it has to be loaded alongside the texture.
                let second = definition
                    .combiner
                    .as_ref()
                    .map(|combiner| combiner.second.as_str());
                let mask = definition
                    .combiner
                    .as_ref()
                    .and_then(|combiner| combiner.mask.as_deref());
                for linked in [
                    definition.texture.as_deref(),
                    definition.opacity.as_deref(),
                    definition.emissive_mask.as_deref(),
                    definition.specular.as_deref(),
                    definition.specular_mask.as_deref(),
                    definition.next_frame.as_deref(),
                    second,
                    mask,
                ]
                .into_iter()
                .flatten()
                {
                    pending.insert(linked.to_ascii_lowercase());
                }
                for alias in aliases {
                    self.materials
                        .entry(alias)
                        .or_insert_with(|| definition.clone());
                }
            }
        }
        for (path, target) in redirects {
            if let Some(material) = self.materials.get(&target).cloned() {
                self.materials.entry(path.clone()).or_insert(material);
            }
            if let Some(texture) = self.textures.get(&target).cloned() {
                self.textures.entry(path).or_insert(texture);
            }
        }
    }

    fn has_mesh(&self, name: &str) -> bool {
        self.meshes.contains_key(&name.to_ascii_lowercase())
    }

    /// Components of emitters that only play once their game event fires.
    pub(super) fn game_event_counts(&self) -> BTreeMap<i32, usize> {
        let mut counts = BTreeMap::new();
        for effect in &self.effects {
            if let Some(event) = effect.game_event {
                *counts.entry(event).or_default() += 1;
            }
        }
        counts
    }

    pub(super) fn resolved_instance_count(&self) -> usize {
        self.instances
            .iter()
            .filter(|instance| self.has_mesh(&instance.mesh))
            .count()
    }

    pub(super) fn unresolved_instance_count(&self) -> usize {
        self.instances.len() - self.resolved_instance_count()
    }
}

impl Default for SceneEnvironment {
    fn default() -> Self {
        Self {
            // A missing Unreal zone color means no hue information is available.
            // Keep the fallback neutral so it changes brightness without tinting
            // every material in the map.
            ambient_color: Color::srgb(1.0, 1.0, 1.0),
            ambient_brightness: 300.0,
            fog: None,
        }
    }
}

pub(super) fn scene_extent(
    terrain: &Option<TerrainGeometry>,
    instances: &[MeshInstance],
) -> (Vec3, f32) {
    let mut minimum = Vec3::splat(f32::INFINITY);
    let mut maximum = Vec3::splat(f32::NEG_INFINITY);
    if let Some(terrain) = terrain {
        for position in &terrain.positions {
            let position = Vec3::from_array(*position);
            minimum = minimum.min(position);
            maximum = maximum.max(position);
        }
    }
    for instance in instances {
        minimum = minimum.min(instance.location);
        maximum = maximum.max(instance.location);
    }
    if !minimum.is_finite() || !maximum.is_finite() {
        return (Vec3::ZERO, UNREAL_UNITS_PER_METER * 128.0);
    }
    let center = (minimum + maximum) * 0.5;
    let radius = (maximum - minimum).max_element() * 0.5;
    (center, radius)
}

pub(super) fn asset_roots(map_path: &Path, configured: &[PathBuf]) -> Vec<PathBuf> {
    let mut roots = configured.to_vec();
    let mut client_directories = configured
        .iter()
        .filter_map(|path| path.parent().map(Path::to_path_buf))
        .collect::<Vec<_>>();
    if let Some(map_directory) = map_path.parent() {
        roots.push(map_directory.to_path_buf());
        if let Some(client_directory) = map_directory.parent() {
            client_directories.push(client_directory.to_path_buf());
        }
    }
    for client_directory in client_directories {
        if let Ok(entries) = fs::read_dir(client_directory) {
            roots.extend(entries.filter_map(Result::ok).filter_map(|entry| {
                let wanted = ["StaticMeshes", "Animations", "Textures", "SysTextures"];
                (entry.file_type().is_ok_and(|file_type| file_type.is_dir())
                    && wanted.iter().any(|name| {
                        entry
                            .file_name()
                            .to_string_lossy()
                            .eq_ignore_ascii_case(name)
                    }))
                .then(|| entry.path())
            }));
        }
    }
    let mut unique = BTreeSet::new();
    roots
        .into_iter()
        .filter(|path| configured.contains(path) || path.exists())
        .filter(|path| unique.insert(fs::canonicalize(path).unwrap_or_else(|_| path.clone())))
        .collect()
}

fn index_asset_packages(roots: &[PathBuf]) -> Result<BTreeMap<String, PathBuf>, String> {
    let mut packages = BTreeMap::new();
    for root in roots {
        collect_asset_packages(root, &mut packages)?;
    }
    Ok(packages)
}

fn collect_asset_packages(
    path: &Path,
    packages: &mut BTreeMap<String, PathBuf>,
) -> Result<(), String> {
    if path.is_file() {
        if path.extension().is_some_and(is_asset_package_extension) {
            let name = path
                .file_stem()
                .and_then(|value| value.to_str())
                .ok_or_else(|| format!("{} has a non-Unicode filename", path.display()))?
                .to_ascii_lowercase();
            packages.entry(name).or_insert_with(|| path.to_path_buf());
        }
        return Ok(());
    }
    let entries = fs::read_dir(path)
        .map_err(|error| format!("cannot search asset path {}: {error}", path.display()))?;
    for entry in entries {
        let entry = entry
            .map_err(|error| format!("cannot search asset path {}: {error}", path.display()))?;
        let file_type = entry
            .file_type()
            .map_err(|error| format!("cannot inspect {}: {error}", entry.path().display()))?;
        if file_type.is_dir() {
            collect_asset_packages(&entry.path(), packages)?;
        } else if file_type.is_file()
            && entry
                .path()
                .extension()
                .is_some_and(is_asset_package_extension)
        {
            let path = entry.path();
            let name = path
                .file_stem()
                .and_then(|value| value.to_str())
                .ok_or_else(|| format!("{} has a non-Unicode filename", path.display()))?
                .to_ascii_lowercase();
            packages.entry(name).or_insert(path);
        }
    }
    Ok(())
}

fn is_asset_package_extension(extension: &std::ffi::OsStr) -> bool {
    extension.eq_ignore_ascii_case("usx")
        || extension.eq_ignore_ascii_case("utx")
        || extension.eq_ignore_ascii_case("ukx")
}

/// Unreal writes both a group of the package being read and an object of
/// another package as `name.object`, so a reference qualified as the former
/// that the package turns out not to hold may well be the latter.
pub(super) fn other_package_reading<T>(
    object_path: &str,
    package_files: &BTreeMap<String, T>,
) -> Option<String> {
    let (package, rest) = object_path.split_once('.')?;
    let other = external_package_name(rest).filter(|other| other != package)?;
    (rest.contains('.') && package_files.contains_key(&other)).then(|| rest.to_owned())
}

pub(super) fn external_package_name(object_path: &str) -> Option<String> {
    object_path
        .split_once('.')
        .map(|(package, _)| package.to_ascii_lowercase())
}

pub(super) fn external_mesh_aliases(package_name: &str, object_path: &str) -> Vec<String> {
    let object_path = object_path.to_ascii_lowercase();
    let qualified = format!("{package_name}.{object_path}");
    if object_path.starts_with(&format!("{package_name}.")) {
        vec![object_path]
    } else {
        vec![object_path, qualified]
    }
}

pub(super) fn qualify_material_paths(package_name: &str, materials: Vec<String>) -> Vec<String> {
    materials
        .into_iter()
        .map(|material| qualify_material_path(package_name, material))
        .collect()
}

fn qualify_material_path(package_name: &str, material: String) -> String {
    if material.is_empty() || material.contains('.') {
        material.to_ascii_lowercase()
    } else {
        format!(
            "{}.{}",
            package_name.to_ascii_lowercase(),
            material.to_ascii_lowercase()
        )
    }
}

fn display_roots(roots: &[PathBuf]) -> String {
    if roots.is_empty() {
        return "(no asset paths)".into();
    }
    roots
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

pub(super) fn resolve_texture(scene: &MapScene, material_name: &str) -> Option<ResolvedTexture> {
    resolve_texture_from(
        &scene.materials,
        |name| scene.textures.contains_key(name),
        material_name,
    )
}

pub(super) fn resolve_texture_from(
    materials: &HashMap<String, MaterialGeometry>,
    texture_exists: impl Fn(&str) -> bool,
    material_name: &str,
) -> Option<ResolvedTexture> {
    resolve_material(materials, &texture_exists, material_name, true)
}

fn resolve_material(
    materials: &HashMap<String, MaterialGeometry>,
    texture_exists: &impl Fn(&str) -> bool,
    material_name: &str,
    follow_opacity: bool,
) -> Option<ResolvedTexture> {
    let mut current = material_name.to_ascii_lowercase();
    let mut visited = BTreeSet::new();
    let mut two_sided = false;
    let mut blend = MaterialBlend::Opaque;
    let mut unlit = false;
    let mut emissive = false;
    let mut opacity = None;
    let mut emissive_mask = None;
    let mut alpha_ref = None;
    let mut specular: Option<(String, Option<String>)> = None;
    let mut motion = UvMotion::default();
    let mut combiner: Option<CombinerGeometry> = None;
    let mut tint = [1.0; 4];
    for _ in 0..32 {
        if !visited.insert(current.clone()) {
            return None;
        }
        let material = materials.get(&current);
        if let Some(material) = material {
            two_sided |= material.two_sided;
            if !matches!(material.blend, MaterialBlend::Opaque) {
                blend = material.blend;
            }
            unlit |= material.unlit;
            emissive |= material.emissive;
            opacity = opacity.or_else(|| material.opacity.clone());
            emissive_mask = emissive_mask.or_else(|| material.emissive_mask.clone());
            alpha_ref = alpha_ref.or(material.alpha_ref);
            specular = specular.or_else(|| {
                material
                    .specular
                    .clone()
                    .map(|layer| (layer, material.specular_mask.clone()))
            });
            motion = motion.then(material.motion);
            combiner = combiner.or_else(|| material.combiner.clone());
            if let Some(color) = material.tint {
                for (channel, modifier) in tint.iter_mut().zip(color) {
                    *channel *= modifier;
                }
            }
        }
        if texture_exists(&current) {
            return Some(ResolvedTexture {
                texture: current,
                two_sided,
                blend,
                unlit,
                emissive,
                opacity: follow_opacity
                    .then(|| resolve_opacity(materials, texture_exists, opacity?))
                    .flatten(),
                emissive_mask: follow_opacity
                    .then(|| {
                        resolve_material(materials, texture_exists, &emissive_mask?, false)
                            .map(|mask| mask.texture)
                    })
                    .flatten(),
                alpha_ref,
                specular: follow_opacity
                    .then(|| {
                        let (layer, mask) = specular?;
                        let resolve = |name: &str| {
                            resolve_material(materials, texture_exists, name, false)
                                .map(|resolved| resolved.texture)
                        };
                        Some(SpecularLayer {
                            texture: resolve(&layer)?,
                            mask: mask.as_deref().and_then(resolve),
                        })
                    })
                    .flatten(),
                motion,
                // A combiner's second material is drawn as its own layer; a
                // combiner nested inside that one is beyond what is drawn.
                second: follow_opacity
                    .then(|| {
                        let combiner = combiner?;
                        let second =
                            resolve_material(materials, texture_exists, &combiner.second, false)?;
                        Some(SecondLayer {
                            texture: second.texture,
                            motion: second.motion,
                            operation: combiner.operation,
                            modulate: combiner.modulate,
                            vertex_mask: combiner
                                .mask
                                .as_ref()
                                .and_then(|mask| materials.get(mask))
                                .is_some_and(|mask| mask.vertex_color),
                        })
                    })
                    .flatten(),
                tint,
            });
        }
        current = material?.texture.as_ref()?.to_ascii_lowercase();
    }
    None
}

// A shader's opacity is another material: a constant colour contributes its
// alpha, anything else contributes the texture it resolves to.
fn resolve_opacity(
    materials: &HashMap<String, MaterialGeometry>,
    texture_exists: &impl Fn(&str) -> bool,
    opacity: String,
) -> Option<MaterialOpacity> {
    let opacity = opacity.to_ascii_lowercase();
    if let Some(color) = materials.get(&opacity).and_then(|material| material.color) {
        return Some(MaterialOpacity::Constant(color[3]));
    }
    let resolved = resolve_material(materials, texture_exists, &opacity, false)?;
    Some(MaterialOpacity::Texture(resolved.texture))
}

fn terrain_geometry(
    width: i32,
    height: i32,
    vertices: Vec<Vector3>,
    colors: Vec<UnrealColor>,
    layers: TerrainLayering,
    topology: TerrainTopology,
) -> Result<TerrainGeometry, String> {
    let width = usize::try_from(width).map_err(|_| "terrain width is negative")?;
    let height = usize::try_from(height).map_err(|_| "terrain height is negative")?;
    if width < 2 || height < 2 || width.checked_mul(height) != Some(vertices.len()) {
        return Err(format!(
            "terrain dimensions {width}x{height} do not match {} vertices",
            vertices.len()
        ));
    }
    let positions = vertices.into_iter().map(unreal_point).collect();
    let colors = if colors.len() == width * height {
        colors.into_iter().map(unreal_color).collect()
    } else {
        vec![[0.38, 0.55, 0.28, 1.0]; width * height]
    };
    Ok(TerrainGeometry {
        width,
        height,
        positions,
        colors,
        layers,
        quad_visibility: topology.quad_visibility,
        edge_turns: topology.edge_turns,
    })
}

pub(super) fn terrain_from_g16_heightmap(
    width: usize,
    height: usize,
    data: &[u8],
    to_world: &Coordinates,
    colors: Vec<UnrealColor>,
    layers: TerrainLayering,
    topology: TerrainTopology,
) -> Result<TerrainGeometry, String> {
    let sample_count = width
        .checked_mul(height)
        .ok_or("terrain dimensions overflow")?;
    let byte_count = sample_count
        .checked_mul(2)
        .ok_or("terrain height-map size overflows")?;
    if width < 2 || height < 2 || data.len() != byte_count {
        return Err(format!(
            "terrain dimensions {width}x{height} require {byte_count} G16 bytes, got {}",
            data.len()
        ));
    }
    let positions = data
        .chunks_exact(2)
        .enumerate()
        .map(|(index, bytes)| {
            let point = Vec3::new(
                (index % width) as f32,
                (index / width) as f32,
                f32::from(u16::from_le_bytes([bytes[0], bytes[1]])),
            );
            unreal_point(transform_by_coordinates(point, to_world)).to_owned()
        })
        .collect();
    let colors = if colors.len() == sample_count {
        colors.into_iter().map(unreal_color).collect()
    } else {
        vec![[0.38, 0.55, 0.28, 1.0]; sample_count]
    };
    Ok(TerrainGeometry {
        width,
        height,
        positions,
        colors,
        layers,
        quad_visibility: topology.quad_visibility,
        edge_turns: topology.edge_turns,
    })
}

fn transform_by_coordinates(point: Vec3, coordinates: &Coordinates) -> Vector3 {
    let relative = point
        - Vec3::new(
            coordinates.origin.x,
            coordinates.origin.y,
            coordinates.origin.z,
        );
    Vector3 {
        x: relative.dot(Vec3::new(
            coordinates.x_axis.x,
            coordinates.x_axis.y,
            coordinates.x_axis.z,
        )),
        y: relative.dot(Vec3::new(
            coordinates.y_axis.x,
            coordinates.y_axis.y,
            coordinates.y_axis.z,
        )),
        z: relative.dot(Vec3::new(
            coordinates.z_axis.x,
            coordinates.z_axis.y,
            coordinates.z_axis.z,
        )),
    }
}

fn mesh_geometry(value: StaticMesh, materials: Vec<String>) -> MeshGeometry {
    let positions = value
        .vertex_stream
        .vertices
        .iter()
        .map(|vertex| unreal_point(vertex.position.clone()))
        .collect();
    let normals = value
        .vertex_stream
        .vertices
        .iter()
        .map(|vertex| unreal_direction(&vertex.normal).to_array())
        .collect();
    let uvs = value
        .uv_streams
        .first()
        .filter(|stream| stream.coordinates.len() == value.vertex_stream.vertices.len())
        .map(|stream| {
            stream
                .coordinates
                .iter()
                .map(|coordinate| [coordinate.u, coordinate.v])
                .collect()
        })
        .unwrap_or_else(|| vec![[0.0, 0.0]; value.vertex_stream.vertices.len()]);
    // A mesh painted fully opaque carries no mask worth drawing.
    let vertex_alpha = Some(&value.alpha_stream.colors)
        .filter(|colors| colors.len() == value.vertex_stream.vertices.len())
        .filter(|colors| colors.iter().any(|color| color.alpha < u8::MAX))
        .map(|colors| {
            colors
                .iter()
                .map(|color| f32::from(color.alpha) / 255.0)
                .collect()
        });
    let indices: Vec<u32> = value
        .primary_indices
        .indices
        .into_iter()
        .map(u32::from)
        .collect();
    let parts = value
        .sections
        .iter()
        .enumerate()
        .filter_map(|(index, section)| {
            let start = usize::from(section.first_index);
            let end = start.checked_add(usize::from(section.face_count) * 3)?;
            let indices = indices.get(start..end)?.to_vec();
            Some(MeshPart {
                indices,
                material: materials
                    .get(index)
                    .filter(|value| !value.is_empty())
                    .cloned(),
                ..default()
            })
        })
        .collect::<Vec<_>>();
    let parts = if parts.is_empty() {
        vec![MeshPart {
            indices,
            material: materials.first().filter(|value| !value.is_empty()).cloned(),
            ..default()
        }]
    } else {
        parts
    };
    MeshGeometry {
        vertex_alpha,
        positions,
        normals,
        uvs,
        parts,
        skin: None,
    }
}

pub(super) fn skeletal_mesh_geometry(value: SkeletalMesh) -> MeshGeometry {
    let materials = value
        .materials
        .iter()
        .map(|material| material.path.clone())
        .collect::<Vec<_>>();
    let Some(lod) = value.lods.into_iter().next() else {
        return MeshGeometry {
            vertex_alpha: None,
            positions: Vec::new(),
            normals: Vec::new(),
            uvs: Vec::new(),
            parts: Vec::new(),
            skin: None,
        };
    };
    let (positions, normals, uvs) = if !lod.lineage_vertices.is_empty() {
        (
            lod.lineage_vertices
                .iter()
                .map(|vertex| unreal_point(vertex.position.clone()))
                .collect(),
            lod.lineage_vertices
                .iter()
                .map(|vertex| unreal_direction(&vertex.normal).to_array())
                .collect(),
            lod.lineage_vertices
                .iter()
                .map(|vertex| [vertex.u, vertex.v])
                .collect(),
        )
    } else {
        let points = &lod.points.values;
        let positions = lod
            .wedges
            .values
            .iter()
            .filter_map(|wedge| points.get(usize::from(wedge.vertex_index)))
            .cloned()
            .map(unreal_point)
            .collect::<Vec<_>>();
        let uvs = lod
            .wedges
            .values
            .iter()
            .map(|wedge| [wedge.u, wedge.v])
            .collect::<Vec<_>>();
        (
            positions.clone(),
            vec![[0.0, 1.0, 0.0]; positions.len()],
            uvs,
        )
    };
    let indices = if !lod.soft_indices.indices.is_empty() {
        lod.soft_indices.indices
    } else {
        lod.rigid_indices.indices
    };
    let sections = if !lod.soft_sections.is_empty() {
        lod.soft_sections
    } else {
        lod.rigid_sections
    };
    let parts = sections
        .iter()
        .filter_map(|section| {
            let start = usize::from(section.first_face) * 3;
            let end = start.checked_add(usize::from(section.face_count) * 3)?;
            Some(MeshPart {
                indices: indices
                    .get(start..end)?
                    .iter()
                    .copied()
                    .map(u32::from)
                    .collect(),
                material: materials
                    .get(usize::from(section.material_index))
                    .filter(|material| !material.is_empty())
                    .cloned(),
                ..default()
            })
        })
        .collect::<Vec<_>>();
    let parts = if parts.is_empty() && !indices.is_empty() {
        vec![MeshPart {
            indices: indices.into_iter().map(u32::from).collect(),
            material: materials.first().filter(|value| !value.is_empty()).cloned(),
            ..default()
        }]
    } else {
        parts
    };
    let skin = (!lod.lineage_vertices.is_empty()).then(|| {
        let mut joint_indices = vec![[0; 4]; lod.lineage_vertices.len()];
        for section in &sections {
            let start = usize::from(section.min_wedge_index);
            let end = usize::from(section.max_wedge_index)
                .saturating_add(1)
                .min(lod.lineage_vertices.len());
            for (target, vertex) in joint_indices[start..end]
                .iter_mut()
                .zip(&lod.lineage_vertices[start..end])
            {
                *target = vertex.bones.map(|bone| {
                    section
                        .bone_map
                        .get(usize::from(bone))
                        .and_then(|bone| u16::try_from(*bone).ok())
                        .unwrap_or(0)
                });
            }
        }
        SkinGeometry {
            bones: value
                .skeleton
                .iter()
                .enumerate()
                .map(|(index, bone)| BoneGeometry {
                    name: bone.name.clone(),
                    parent: (bone.parent_index >= 0 && bone.parent_index as usize != index)
                        .then_some(bone.parent_index as usize),
                    transform: bone_transform(&bone.position, &bone.orientation),
                })
                .collect(),
            joint_indices,
            joint_weights: lod
                .lineage_vertices
                .iter()
                .map(|vertex| vertex.weights)
                .collect(),
            animation: value.animation.path.to_ascii_lowercase(),
        }
    });
    MeshGeometry {
        vertex_alpha: None,
        positions,
        normals,
        uvs,
        parts,
        skin,
    }
}

pub(super) fn animation_geometry(value: MeshAnimation) -> AnimationGeometry {
    let Some(move_data) = value.moves.into_iter().next() else {
        return AnimationGeometry {
            duration: 0.0,
            tracks: Vec::new(),
        };
    };
    let rate = value
        .sequences
        .first()
        .map(|sequence| sequence.rate)
        .filter(|rate| *rate > 0.0)
        .unwrap_or(30.0);
    let mut tracks = (0..value.bones.len()).map(|_| None).collect::<Vec<_>>();
    for (bone, track) in move_data.bone_indices.into_iter().zip(move_data.tracks) {
        let Ok(bone) = usize::try_from(bone) else {
            continue;
        };
        let frame_count = track.orientations.len().max(track.positions.len()).max(1);
        let times = if track.times.len() == frame_count {
            track.times.into_iter().map(|time| time / rate).collect()
        } else {
            (0..frame_count).map(|frame| frame as f32 / rate).collect()
        };
        let rotations = track.orientations.iter().map(unreal_quaternion).collect();
        let translations = track
            .positions
            .into_iter()
            .map(|position| unreal_direction_vec(Vec3::new(position.x, position.y, position.z)))
            .map(|position| position / UNREAL_UNITS_PER_METER)
            .collect();
        if let Some(slot) = tracks.get_mut(bone) {
            *slot = Some(AnimationTrackGeometry {
                times,
                rotations,
                translations,
            });
        }
    }
    AnimationGeometry {
        duration: (move_data.track_time / rate).max(1.0 / rate),
        tracks,
    }
}

fn bone_transform(
    position: &Vector3,
    orientation: &unreal_package::native::Quaternion,
) -> Transform {
    Transform {
        translation: unreal_direction_vec(Vec3::new(position.x, position.y, position.z))
            / UNREAL_UNITS_PER_METER,
        rotation: unreal_quaternion(orientation),
        ..default()
    }
}

fn unreal_quaternion(value: &unreal_package::native::Quaternion) -> Quat {
    let basis = Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2);
    (basis * Quat::from_xyzw(value.x, value.y, value.z, value.w) * basis.inverse()).normalize()
}

fn bsp_geometry(bytes: &[u8], object_paths: &HashMap<i32, String>) -> Result<MeshGeometry, String> {
    struct Node {
        vertex_pool: usize,
        surface: usize,
        vertex_count: usize,
    }
    struct Surface {
        material: Option<String>,
        poly_flags: u32,
        base: usize,
        normal: usize,
        texture_u: usize,
        texture_v: usize,
    }
    let mut reader = BinaryReader::new(bytes);
    let mut vectors = Vec::new();
    for _ in 0..reader.compact_count().map_err(codec_error)? {
        vectors.push(read_vector(&mut reader)?);
    }
    let mut points = Vec::new();
    for _ in 0..reader.compact_count().map_err(codec_error)? {
        points.push(read_vector(&mut reader)?);
    }
    let mut nodes = Vec::new();
    for _ in 0..reader.compact_count().map_err(codec_error)? {
        for _ in 0..4 {
            reader.f32().map_err(codec_error)?;
        }
        reader.take(9).map_err(codec_error)?;
        let vertex_pool = nonnegative(reader.compact_int().map_err(codec_error)?.get())?;
        let surface = nonnegative(reader.compact_int().map_err(codec_error)?.get())?;
        for _ in 0..5 {
            reader.compact_int().map_err(codec_error)?;
        }
        reader.take(32).map_err(codec_error)?;
        reader.take(2).map_err(codec_error)?;
        let vertex_count = usize::from(reader.u8().map_err(codec_error)?);
        reader.take(8).map_err(codec_error)?;
        reader.take(12).map_err(codec_error)?;
        nodes.push(Node {
            vertex_pool,
            surface,
            vertex_count,
        });
    }
    let mut surfaces = Vec::new();
    for _ in 0..reader.compact_count().map_err(codec_error)? {
        let material_reference = reader.compact_int().map_err(codec_error)?.get();
        let poly_flags = reader.u32().map_err(codec_error)?;
        let base = nonnegative(reader.compact_int().map_err(codec_error)?.get())?;
        let normal = nonnegative(reader.compact_int().map_err(codec_error)?.get())?;
        let texture_u = nonnegative(reader.compact_int().map_err(codec_error)?.get())?;
        let texture_v = nonnegative(reader.compact_int().map_err(codec_error)?.get())?;
        reader.compact_int().map_err(codec_error)?;
        reader.compact_int().map_err(codec_error)?;
        reader.take(20).map_err(codec_error)?;
        reader.take(4).map_err(codec_error)?;
        surfaces.push(Surface {
            material: object_paths.get(&material_reference).cloned(),
            poly_flags,
            base,
            normal,
            texture_u,
            texture_v,
        });
    }
    let mut vertex_pool = Vec::new();
    for _ in 0..reader.compact_count().map_err(codec_error)? {
        let point = nonnegative(reader.compact_int().map_err(codec_error)?.get())?;
        reader.compact_int().map_err(codec_error)?;
        vertex_pool.push(point);
    }
    let mut positions = Vec::new();
    let mut normals = Vec::new();
    let mut uvs = Vec::new();
    let mut parts: HashMap<(Option<String>, u32), Vec<u32>> = HashMap::new();
    for node in nodes {
        if node.vertex_count < 3 {
            continue;
        }
        let surface = surfaces
            .get(node.surface)
            .ok_or("BSP surface index is invalid")?;
        let base = points
            .get(surface.base)
            .ok_or("BSP texture base is invalid")?;
        let normal = vectors
            .get(surface.normal)
            .ok_or("BSP normal index is invalid")?;
        let texture_u = vectors
            .get(surface.texture_u)
            .ok_or("BSP U vector is invalid")?;
        let texture_v = vectors
            .get(surface.texture_v)
            .ok_or("BSP V vector is invalid")?;
        let first = u32::try_from(positions.len()).map_err(|_| "BSP has too many vertices")?;
        for pool_index in node.vertex_pool..node.vertex_pool + node.vertex_count {
            let point = points
                .get(
                    *vertex_pool
                        .get(pool_index)
                        .ok_or("BSP vertex pool index is invalid")?,
                )
                .ok_or("BSP point index is invalid")?;
            positions.push(unreal_point(point.clone()));
            normals.push(unreal_direction(normal).to_array());
            let relative = Vec3::new(point.x - base.x, point.y - base.y, point.z - base.z);
            uvs.push([
                relative.dot(Vec3::new(texture_u.x, texture_u.y, texture_u.z)) / 256.0,
                relative.dot(Vec3::new(texture_v.x, texture_v.y, texture_v.z)) / 256.0,
            ]);
        }
        let indices = parts
            .entry((surface.material.clone(), surface.poly_flags))
            .or_default();
        for triangle in 1..node.vertex_count - 1 {
            indices.extend_from_slice(&[
                first,
                first + triangle as u32,
                first + triangle as u32 + 1,
            ]);
        }
    }
    Ok(MeshGeometry {
        vertex_alpha: None,
        positions,
        normals,
        uvs,
        parts: parts
            .into_iter()
            .filter(|((material, poly_flags), _)| {
                should_render_bsp_surface(*poly_flags, material.as_deref())
            })
            .map(|((material, poly_flags), indices)| MeshPart {
                indices,
                material,
                poly_flags,
            })
            .collect(),
        skin: None,
    })
}

pub(super) fn should_render_bsp_surface(poly_flags: u32, material: Option<&str>) -> bool {
    const PF_INVISIBLE: u32 = 0x0000_0001;
    // A fake backdrop is a hole onto the sky zone, not a surface: Unreal draws
    // the sky through it rather than the texture it carries.
    const PF_FAKE_BACKDROP: u32 = 0x0000_0080;

    poly_flags & (PF_INVISIBLE | PF_FAKE_BACKDROP) == 0 && !is_zone_portal_material(material)
}

pub(super) fn is_zone_portal_material(material: Option<&str>) -> bool {
    material.is_some_and(|material| {
        material
            .rsplit(['.', '/'])
            .next()
            .is_some_and(|name| name.eq_ignore_ascii_case("ZonePortal"))
    })
}

fn read_vector(reader: &mut BinaryReader<'_>) -> Result<Vector3, String> {
    Ok(Vector3 {
        x: reader.f32().map_err(codec_error)?,
        y: reader.f32().map_err(codec_error)?,
        z: reader.f32().map_err(codec_error)?,
    })
}

fn nonnegative(value: i32) -> Result<usize, String> {
    usize::try_from(value).map_err(|_| "negative BSP index".into())
}

fn codec_error(error: impl std::fmt::Debug) -> String {
    format!("{error:?}")
}

fn mesh_materials(properties: &[PropertyDump]) -> Vec<String> {
    property(properties, "Materials")
        .and_then(|property| serde_json::from_str(&property.value).ok())
        .unwrap_or_default()
}

pub(super) fn terrain_layers(properties: &[PropertyDump]) -> TerrainLayering {
    let quad_size = property(properties, "TerrainScale")
        .and_then(|property| triplet(&property.value))
        .map_or(Vec2::ONE, |scale| Vec2::new(scale.x, scale.y));
    let layers = properties
        .iter()
        .filter(|property| property.name == "Layers")
        .filter_map(|property| serde_json::from_str::<serde_json::Value>(&property.value).ok())
        .filter_map(|layer| {
            let text = |name: &str| {
                layer
                    .get(name)
                    .and_then(|value| value.as_str())
                    .filter(|value| !value.is_empty())
            };
            let scale = |name: &str| {
                text(name)
                    .and_then(|value| value.parse::<f32>().ok())
                    .unwrap_or(1.0)
            };
            Some(TerrainLayer {
                texture: text("Texture")?.to_owned(),
                alpha_map: text("AlphaMap").map(str::to_owned),
                scale: Vec2::new(scale("UScale"), scale("VScale")),
            })
        })
        .collect();
    TerrainLayering { quad_size, layers }
}

fn terrain_bitmap(properties: &[PropertyDump], name: &str) -> Result<Vec<u8>, String> {
    let Some(property) = property(properties, name) else {
        return Ok(Vec::new());
    };
    let bytes = decode_hex_bytes(&property.raw)
        .map_err(|error| format!("cannot decode terrain {name}: {error}"))?;
    let mut reader = BinaryReader::new(&bytes);
    let count = reader
        .compact_count()
        .map_err(|error| format!("cannot decode terrain {name} length: {error:?}"))?;
    let byte_count = count
        .checked_mul(std::mem::size_of::<u32>())
        .ok_or_else(|| format!("terrain {name} length overflows"))?;
    Ok(reader
        .take(byte_count)
        .map_err(|error| format!("cannot decode terrain {name} data: {error:?}"))?
        .to_vec())
}

fn decode_hex_bytes(value: &str) -> Result<Vec<u8>, String> {
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

fn material_geometry(package_name: &str, export: &ExportDump) -> Option<MaterialGeometry> {
    let direct_texture = matches!(
        export.native_object.as_ref(),
        Some(NativeObject::Texture(_))
    )
    .then(|| qualify_object_path(package_name, &export.full_name));
    let linked_texture = [
        "Diffuse",
        "Material",
        "Material1",
        "Texture",
        "SelfIllumination",
        "Specular",
        "Detail",
    ]
    .iter()
    .find_map(|name| property(&export.properties, name))
    .filter(|property| !property.value.is_empty())
    .map(|property| qualify_object_path(package_name, &property.value));
    let texture = direct_texture.or(linked_texture);
    let color = constant_color(export);
    let vertex_color = export.class.ends_with("VertexColor");
    if texture.is_none() && color.is_none() && !vertex_color {
        return None;
    }
    let blend = material_blend(&export.properties);
    Some(MaterialGeometry {
        texture,
        two_sided: material_is_two_sided(&export.properties),
        blend,
        unlit: flag(&export.properties, "Unlit") || flag(&export.properties, "bUnlit"),
        emissive: property(&export.properties, "SelfIllumination").is_some()
            || matches!(blend, MaterialBlend::Add),
        opacity: material_link(package_name, &export.properties, "Opacity"),
        alpha_ref: property(&export.properties, "AlphaRef")
            .and_then(|property| property.value.parse::<f32>().ok())
            .map(|alpha| alpha / 255.0),
        emissive_mask: material_link(package_name, &export.properties, "SelfIlluminationMask"),
        specular: material_link(package_name, &export.properties, "Specular")
            .filter(|_| export.class.ends_with("Shader")),
        specular_mask: material_link(package_name, &export.properties, "SpecularityMask"),
        next_frame: material_link(package_name, &export.properties, "AnimNext"),
        frame_rate: ["MaxFrameRate", "MinFrameRate"]
            .into_iter()
            .find_map(|name| {
                property(&export.properties, name)
                    .and_then(|property| property.value.parse::<f32>().ok())
                    .filter(|rate| *rate > 0.0)
            })
            .unwrap_or(0.0),
        color,
        tint: color_modifier(export),
        motion: texture_motion(&export.properties),
        combiner: combiner_geometry(package_name, export),
        vertex_color,
    })
}

/// An animated texture's frames in order, following Unreal's AnimNext chain
/// from the first frame until it loops.
pub(super) fn texture_frames(scene: &MapScene, first: &str) -> (Vec<String>, f32) {
    frame_chain(
        &scene.materials,
        |name| scene.textures.contains_key(name),
        first,
    )
}

pub(super) fn frame_chain(
    materials: &HashMap<String, MaterialGeometry>,
    texture_exists: impl Fn(&str) -> bool,
    first: &str,
) -> (Vec<String>, f32) {
    let mut frames = vec![first.to_ascii_lowercase()];
    let rate = materials
        .get(&frames[0])
        .map_or(0.0, |material| material.frame_rate);
    while frames.len() < 256 {
        let Some(next) = materials
            .get(frames.last().unwrap())
            .and_then(|material| material.next_frame.as_deref())
            .map(str::to_ascii_lowercase)
        else {
            break;
        };
        if frames.contains(&next) || !texture_exists(&next) {
            break;
        }
        frames.push(next);
    }
    (frames, rate)
}

fn material_link(package_name: &str, properties: &[PropertyDump], name: &str) -> Option<String> {
    property(properties, name)
        .filter(|property| !property.value.is_empty() && property.property_type == "Object")
        .map(|property| qualify_object_path(package_name, &property.value))
}

fn constant_color(export: &ExportDump) -> Option<[f32; 4]> {
    material_color(export, "ConstantColor")
}

// An Engine.ColorModifier paints whatever material it wraps.
fn color_modifier(export: &ExportDump) -> Option<[f32; 4]> {
    material_color(export, "ColorModifier")
}

fn material_color(export: &ExportDump, class: &str) -> Option<[f32; 4]> {
    if !export.class.ends_with(class) {
        return None;
    }
    let mut channels = property(&export.properties, "Color")?
        .value
        .split(',')
        .map(|channel| channel.trim().parse::<u8>());
    let mut color = [0.0; 4];
    for channel in &mut color {
        *channel = f32::from(channels.next()?.ok()?) / 255.0;
    }
    Some(color)
}

// An Engine.TexPanner scrolls its material along the direction its rotator
// points, a rate of one being a full tile per second. An Engine.TexOscillator
// wobbles it instead, each axis at its own rate and reach.
fn texture_motion(properties: &[PropertyDump]) -> UvMotion {
    let number = |name: &str| {
        property(properties, name)
            .and_then(|property| property.value.parse::<f32>().ok())
            .unwrap_or(0.0)
    };
    let yaw = property(properties, "PanDirection")
        .and_then(|property| triplet(&property.value))
        .map_or(0.0, |direction| {
            direction.y * std::f32::consts::TAU / 65_536.0
        });
    let rate = number("PanRate");
    UvMotion {
        pan: Vec2::new(rate * yaw.cos(), rate * yaw.sin()),
        oscillation_rate: Vec2::new(number("UOscillationRate"), number("VOscillationRate")),
        oscillation_amplitude: Vec2::new(
            number("UOscillationAmplitude"),
            number("VOscillationAmplitude"),
        ),
    }
}

// An Engine.Combiner mixes its first material with a second one, following
// Unreal's EColorOperation and brightened when the combiner asks for it.
fn combiner_geometry(package_name: &str, export: &ExportDump) -> Option<CombinerGeometry> {
    if !export.class.ends_with("Combiner") {
        return None;
    }
    let second =
        property(&export.properties, "Material2").filter(|property| !property.value.is_empty())?;
    Some(CombinerGeometry {
        second: qualify_object_path(package_name, &second.value),
        operation: property(&export.properties, "CombineOperation")
            .and_then(|property| property.value.parse().ok())
            .unwrap_or(0),
        mask: material_link(package_name, &export.properties, "Mask"),
        modulate: if flag(&export.properties, "Modulate4X") {
            4.0
        } else if flag(&export.properties, "Modulate2X") {
            2.0
        } else {
            1.0
        },
    })
}

pub(super) fn material_is_two_sided(properties: &[PropertyDump]) -> bool {
    ["TreatAsTwoSided", "TwoSided", "bTwoSided"]
        .into_iter()
        .any(|name| flag(properties, name))
}

pub(super) fn material_blend(properties: &[PropertyDump]) -> MaterialBlend {
    let output_blending = property(properties, "OutputBlending")
        .and_then(|property| property.value.parse::<u8>().ok());
    match output_blending {
        Some(1) => MaterialBlend::Mask,
        Some(3) => MaterialBlend::Blend,
        Some(5) => MaterialBlend::Add,
        _ if flag(properties, "AlphaTest")
            || flag(properties, "Masked")
            || flag(properties, "bMasked") =>
        {
            MaterialBlend::Mask
        }
        _ if property(properties, "Opacity").is_some()
            || property(properties, "FrameBufferBlending").is_some() =>
        {
            MaterialBlend::Blend
        }
        _ => MaterialBlend::Opaque,
    }
}

pub(super) fn qualify_object_path(package_name: &str, object_path: &str) -> String {
    let object_path = object_path.to_ascii_lowercase();
    if package_name.is_empty() || is_package_qualified(&object_path, package_name) {
        return object_path;
    }
    format!("{}.{object_path}", package_name.to_ascii_lowercase())
}

// A reference out of the package being read already names its own package, so
// it must not be given this one's. Unreal writes an object's path as
// package.group.name, which a reference within a package never reaches.
fn is_package_qualified(object_path: &str, package_name: &str) -> bool {
    object_path.starts_with(&format!("{}.", package_name.to_ascii_lowercase()))
        || object_path.matches('.').count() >= 2
}

/// The colours of an 8-bit texture, which Unreal keeps in a separate palette
/// object. A texture without a Format property is one of these.
fn texture_palette(
    package_name: &str,
    properties: &[PropertyDump],
    exports: &[ExportDump],
) -> Option<Vec<[u8; 4]>> {
    if property(properties, "Format").is_some_and(|format| format.value != "0") {
        return None;
    }
    let palette = qualify_object_path(package_name, &property(properties, "Palette")?.value);
    let export = exports
        .iter()
        .find(|export| qualify_object_path(package_name, &export.full_name) == palette)?;
    decode_palette(&decode_hex_bytes(&export.unread_data).ok()?)
}

/// An Unreal palette: a compact colour count, then the colours as BGRA.
pub(super) fn decode_palette(data: &[u8]) -> Option<Vec<[u8; 4]>> {
    let (count, mut offset) = compact_index(data)?;
    let mut colors = Vec::with_capacity(count);
    for _ in 0..count {
        let bgra = data.get(offset..offset + 4)?;
        colors.push([bgra[2], bgra[1], bgra[0], bgra[3]]);
        offset += 4;
    }
    Some(colors)
}

fn compact_index(data: &[u8]) -> Option<(usize, usize)> {
    let first = *data.first()?;
    let mut value = usize::from(first & 0x3f);
    let mut offset = 1;
    let mut more = first & 0x40 != 0;
    let mut shift = 6;
    while more {
        let byte = *data.get(offset)?;
        value |= usize::from(byte & 0x7f) << shift;
        more = byte & 0x80 != 0;
        shift += 7;
        offset += 1;
    }
    (first & 0x80 == 0).then_some((value, offset))
}

fn paletted_texture_geometry(
    texture: unreal_package::native::Texture,
    palette: &[[u8; 4]],
) -> Option<TextureGeometry> {
    let mip = texture.mipmaps.into_iter().next()?;
    let width = u32::try_from(mip.width).ok()?;
    let height = u32::try_from(mip.height).ok()?;
    if mip.data.len() != (width * height) as usize {
        return None;
    }
    let rgba = mip
        .data
        .iter()
        .flat_map(|index| {
            palette
                .get(usize::from(*index))
                .copied()
                .unwrap_or_default()
        })
        .collect();
    Some(TextureGeometry {
        width,
        height,
        rgba,
    })
}

pub(super) fn texture_geometry(
    properties: &[PropertyDump],
    texture: unreal_package::native::Texture,
) -> Option<TextureGeometry> {
    let format = property(properties, "Format")?.value.parse::<u8>().ok()?;
    let Some(mip) = texture.mipmaps.into_iter().next() else {
        return mip_zero_texture(properties, format);
    };
    let width = u32::try_from(mip.width).ok()?;
    let height = u32::try_from(mip.height).ok()?;
    let rgba = decode_texture(format, width, height, &mip.data)?;
    Some(TextureGeometry {
        width,
        height,
        rgba,
    })
}

pub(super) fn mip_zero_texture(properties: &[PropertyDump], format: u8) -> Option<TextureGeometry> {
    if format != 5 {
        return None;
    }
    let raw = &property(properties, "MipZero")?.raw;
    if raw.len() != 8 {
        return None;
    }
    let mut bgra = [0_u8; 4];
    for (index, byte) in bgra.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&raw[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(TextureGeometry {
        width: 1,
        height: 1,
        rgba: vec![bgra[2], bgra[1], bgra[0], bgra[3]],
    })
}

pub(super) fn decode_texture(format: u8, width: u32, height: u32, data: &[u8]) -> Option<Vec<u8>> {
    let pixel_count = usize::try_from(width.checked_mul(height)?).ok()?;
    if format == 5 && data.len() == pixel_count * 4 {
        let mut rgba = data.to_vec();
        for pixel in rgba.chunks_exact_mut(4) {
            pixel.swap(0, 2);
        }
        return Some(rgba);
    }
    let (block_size, decode): (usize, BlockDecoder) = match format {
        3 => (8, bcdec_rs::bc1),
        7 => (16, bcdec_rs::bc2),
        8 => (16, bcdec_rs::bc3),
        _ => return None,
    };
    let blocks_x = width.div_ceil(4) as usize;
    let blocks_y = height.div_ceil(4) as usize;
    if data.len() < blocks_x.checked_mul(blocks_y)?.checked_mul(block_size)? {
        return None;
    }
    let mut rgba = vec![0; pixel_count * 4];
    let mut block_rgba = [0; 64];
    for block_y in 0..blocks_y {
        for block_x in 0..blocks_x {
            let offset = (block_y * blocks_x + block_x) * block_size;
            decode(&data[offset..offset + block_size], &mut block_rgba, 16);
            for y in 0..4 {
                for x in 0..4 {
                    let target_x = block_x * 4 + x;
                    let target_y = block_y * 4 + y;
                    if target_x < width as usize && target_y < height as usize {
                        let source = (y * 4 + x) * 4;
                        let target = (target_y * width as usize + target_x) * 4;
                        rgba[target..target + 4].copy_from_slice(&block_rgba[source..source + 4]);
                    }
                }
            }
        }
    }
    Some(rgba)
}

pub(super) fn repeating_texture_image(texture: &TextureGeometry) -> Image {
    repeating_rgba_image(texture.width, texture.height, texture.rgba.clone())
}

// An Unreal shader keeps its transparency in a separate material. Bevy's
// standard material has one alpha channel, so the mask is sampled into a copy
// of the texture being drawn; the two are authored at different sizes.
pub(super) fn masked_texture_image(texture: &TextureGeometry, mask: &TextureGeometry) -> Image {
    let mut rgba = texture.rgba.clone();
    for (index, pixel) in rgba.chunks_exact_mut(4).enumerate() {
        let Some(source) = mask_pixel(texture, mask, index) else {
            continue;
        };
        // A mask without an alpha channel of its own masks by brightness.
        pixel[3] = if source[3] == u8::MAX {
            source[0].max(source[1]).max(source[2])
        } else {
            source[3]
        };
    }
    repeating_rgba_image(texture.width, texture.height, rgba)
}

// A shader with a constant opacity takes its transparency from that constant
// alone, the same way a texture opacity replaces rather than scales the alpha
// of the texture it draws.
pub(super) fn opaque_texture_image(texture: &TextureGeometry) -> Image {
    let mut rgba = texture.rgba.clone();
    for pixel in rgba.chunks_exact_mut(4) {
        pixel[3] = u8::MAX;
    }
    repeating_rgba_image(texture.width, texture.height, rgba)
}

// Unreal's SelfIlluminationMask lights a surface only where the mask's alpha
// says so: a stone wall glows at its window panes and nowhere else. Bevy
// multiplies emission by one texture, so the mask is folded into its colour.
pub(super) fn self_illumination_image(texture: &TextureGeometry, mask: &TextureGeometry) -> Image {
    let mut rgba = texture.rgba.clone();
    for (index, pixel) in rgba.chunks_exact_mut(4).enumerate() {
        let strength = mask_pixel(texture, mask, index).map_or(0, |source| source[3]);
        for channel in &mut pixel[..3] {
            *channel = (u16::from(*channel) * u16::from(strength) / 255) as u8;
        }
        pixel[3] = u8::MAX;
    }
    repeating_rgba_image(texture.width, texture.height, rgba)
}

// The mask texel under a texture's pixel; the two are authored at different
// sizes, so the mask is sampled at the same relative position.
fn mask_pixel<'a>(
    texture: &TextureGeometry,
    mask: &'a TextureGeometry,
    index: usize,
) -> Option<&'a [u8]> {
    let index = index as u32;
    let x = index % texture.width * mask.width / texture.width;
    let y = index / texture.width * mask.height / texture.height;
    let start = ((y * mask.width + x) * 4) as usize;
    mask.rgba.get(start..start + 4)
}

pub(super) fn radar_atlas_image(atlas: &RadarAtlas) -> Image {
    let mut image = Image::new(
        Extent3d {
            width: (RADAR_PAGE_SIZE * RADAR_COLUMNS) as u32,
            height: (RADAR_PAGE_SIZE * RADAR_ROWS) as u32,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        atlas.rgba.clone(),
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    );
    image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        mag_filter: bevy::image::ImageFilterMode::Linear,
        min_filter: bevy::image::ImageFilterMode::Linear,
        ..default()
    });
    image
}

fn repeating_rgba_image(width: u32, height: u32, rgba: Vec<u8>) -> Image {
    let mut image = Image::new(
        Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        rgba,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    );
    image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::Repeat,
        address_mode_v: ImageAddressMode::Repeat,
        mag_filter: bevy::image::ImageFilterMode::Linear,
        min_filter: bevy::image::ImageFilterMode::Linear,
        mipmap_filter: bevy::image::ImageFilterMode::Linear,
        ..default()
    });
    image
}

pub(super) fn material_cull_mode(two_sided: bool, mirrored: bool) -> Option<Face> {
    match (two_sided, mirrored) {
        (true, _) => None,
        (false, true) => Some(Face::Back),
        (false, false) => Some(Face::Front),
    }
}

pub(super) fn standard_material(
    resolved: &ResolvedTexture,
    texture: Handle<Image>,
    glow: Option<Handle<Image>>,
    mirrored: bool,
    material_override: MaterialOverride,
) -> StandardMaterial {
    let two_sided = resolved.two_sided || material_override.two_sided;
    let blend = if material_override.alpha_blend {
        MaterialBlend::Blend
    } else {
        resolved.blend
    };
    StandardMaterial {
        base_color: Color::srgba(
            resolved.tint[0],
            resolved.tint[1],
            resolved.tint[2],
            match resolved.opacity {
                Some(MaterialOpacity::Constant(alpha)) => alpha * resolved.tint[3],
                _ => resolved.tint[3],
            },
        ),
        base_color_texture: Some(texture.clone()),
        emissive: if resolved.emissive {
            LinearRgba::WHITE
        } else {
            LinearRgba::BLACK
        },
        emissive_texture: resolved.emissive.then(|| glow.unwrap_or(texture)),
        unlit: resolved.unlit,
        perceptual_roughness: 0.85,
        reflectance: 0.25,
        double_sided: two_sided,
        cull_mode: material_cull_mode(two_sided, mirrored),
        alpha_mode: match blend {
            // A shader that alpha tests is a cutout even when its opacity
            // comes from another material, as Lineage's foliage cards do.
            MaterialBlend::Mask => AlphaMode::Mask(resolved.alpha_ref.unwrap_or(0.35)),
            // Otherwise an opacity material makes the surface translucent
            // whatever the alpha of the texture it draws says.
            _ if resolved.opacity.is_some() => AlphaMode::Blend,
            MaterialBlend::Opaque => AlphaMode::Opaque,
            MaterialBlend::Blend => AlphaMode::Blend,
            MaterialBlend::Add => AlphaMode::Add,
        },
        ..default()
    }
}

fn mesh_instance(export: &ExportDump) -> Option<MeshInstance> {
    let mesh = ["StaticMesh", "SkeletalMesh", "Mesh"]
        .into_iter()
        .find_map(|name| property(&export.properties, name))?
        .value
        .clone();
    let location = property(&export.properties, "Location")
        .and_then(|value| triplet(&value.value))
        .unwrap_or(Vec3::ZERO);
    let rotation = property(&export.properties, "Rotation")
        .and_then(|value| triplet(&value.value))
        .unwrap_or(Vec3::ZERO);
    let uniform_scale = property(&export.properties, "DrawScale")
        .and_then(|value| value.value.parse().ok())
        .unwrap_or(1.0);
    let scale = property(&export.properties, "DrawScale3D")
        .and_then(|value| triplet(&value.value))
        .unwrap_or(Vec3::ONE)
        * uniform_scale;
    Some(MeshInstance {
        name: export.name.clone(),
        mesh,
        location: unreal_direction_vec(location),
        rotation,
        scale: Vec3::new(scale.x, scale.z, scale.y),
        material_override: texture_modify_override(&export.properties),
    })
}

pub(super) fn texture_modify_override(properties: &[PropertyDump]) -> MaterialOverride {
    let Some(value) = property(properties, "TexModifyInfo")
        .and_then(|property| serde_json::from_str::<serde_json::Value>(&property.value).ok())
    else {
        return MaterialOverride::default();
    };
    let flag = |name: &str| {
        value.get(name).is_some_and(|value| {
            value.as_bool().unwrap_or_else(|| {
                value
                    .as_str()
                    .is_some_and(|value| value.eq_ignore_ascii_case("true"))
            })
        })
    };
    if !flag("bUseModify") || flag("bDummy") {
        return MaterialOverride::default();
    }
    MaterialOverride {
        alpha_blend: flag("bAlphaBlend"),
        two_sided: flag("bTwoSide"),
    }
}

pub(super) fn player_start_number(name: &str) -> u32 {
    name.strip_prefix("PlayerStart")
        .and_then(|number| number.parse().ok())
        .unwrap_or(u32::MAX)
}

pub(super) fn is_scene_light_class(class: &str) -> bool {
    matches!(
        class,
        "Engine.Light" | "Engine.NMovableSunLight" | "Gameplay.Spotlight" | "Gameplay.Sunlight"
    )
}

pub(super) fn scene_light(export: &ExportDump) -> SceneLight {
    let number = |name: &str, default: f32| {
        property(&export.properties, name)
            .and_then(|property| property.value.parse().ok())
            .unwrap_or(default)
    };
    let hue = number("LightHue", 0.0) * 360.0 / 255.0;
    let saturation = number("LightSaturation", 0.0) / 255.0;
    SceneLight {
        name: export.name.clone(),
        kind: match export.class.as_str() {
            "Engine.NMovableSunLight" | "Gameplay.Sunlight" => SceneLightKind::Sun,
            "Gameplay.Spotlight" => SceneLightKind::Spot,
            _ => SceneLightKind::Point,
        },
        location: property(&export.properties, "Location")
            .and_then(|property| triplet(&property.value))
            .map(unreal_direction_vec)
            .unwrap_or(Vec3::ZERO),
        rotation: property(&export.properties, "Rotation")
            .and_then(|property| triplet(&property.value))
            .unwrap_or(Vec3::ZERO),
        color: Color::hsl(hue, saturation, 0.5),
        brightness: number("LightBrightness", 64.0),
        radius: number("LightRadius", 64.0),
        cone: number("LightCone", 128.0),
    }
}

// Unreal scrolls a surface's texture only where the mapper asked for it.
pub(super) const PF_AUTO_U_PAN: u32 = 0x0000_0200;
pub(super) const PF_AUTO_V_PAN: u32 = 0x0000_0400;

// An Unreal sky is a room built far outside the playable area and viewed from
// its own zone actor. Everything the level's BSP holds around that actor is
// the sky, so it is drawn by a camera that only ever turns.
const SKY_ZONE_RADIUS: f32 = 65_536.0;

fn sky_zone_view(export: &ExportDump) -> Option<SkyZone> {
    let origin = property(&export.properties, "Location")
        .and_then(|property| triplet(&property.value))
        .map(unreal_direction_vec)?;
    Some(SkyZone {
        origin,
        scale: property(&export.properties, "DrawScale")
            .and_then(|property| property.value.parse().ok())
            .unwrap_or(1.0),
        pan: Vec2::new(
            property(&export.properties, "TexUPanSpeed")
                .and_then(|property| property.value.parse().ok())
                .unwrap_or(0.0),
            property(&export.properties, "TexVPanSpeed")
                .and_then(|property| property.value.parse().ok())
                .unwrap_or(0.0),
        ),
    })
}

// Lineage hangs its sun and moon in the sky room as actors wearing a single
// skin, sized by the radius they are drawn at.
fn sky_body(export: &ExportDump) -> Option<SkyBody> {
    let texture = property(&export.properties, "Skins")
        .and_then(|property| serde_json::from_str::<Vec<String>>(&property.value).ok())?
        .into_iter()
        .find(|skin| !skin.is_empty())?;
    Some(SkyBody {
        name: export.name.clone(),
        night: export.class.ends_with("NMoon"),
        texture,
        location: property(&export.properties, "Location")
            .and_then(|property| triplet(&property.value))
            .map(unreal_direction_vec)?,
        radius: property(&export.properties, "Radius")
            .and_then(|property| property.value.parse().ok())
            .unwrap_or(128.0),
    })
}

fn split_sky_geometry(
    bsp: &mut Vec<MeshGeometry>,
    zone: SkyZone,
    bodies: Vec<SkyBody>,
) -> SkyGeometry {
    let mut parts = Vec::new();
    for geometry in bsp.iter_mut() {
        let sky: Vec<_> = geometry
            .parts
            .extract_if(.., |part| {
                part.indices.iter().all(|index| {
                    let position = Vec3::from_array(geometry.positions[*index as usize]);
                    position.distance(zone.origin) <= SKY_ZONE_RADIUS
                })
            })
            .collect();
        if sky.is_empty() {
            continue;
        }
        parts.push(MeshGeometry {
            vertex_alpha: None,
            positions: geometry.positions.clone(),
            normals: geometry.normals.clone(),
            uvs: geometry.uvs.clone(),
            parts: sky,
            skin: None,
        });
    }
    bsp.retain(|geometry| !geometry.parts.is_empty());
    let mut bounds = (Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY));
    for geometry in &parts {
        for index in geometry.parts.iter().flat_map(|part| &part.indices) {
            let Some(position) = geometry.positions.get(*index as usize) else {
                continue;
            };
            let position = Vec3::from_array(*position) - zone.origin;
            bounds.0 = bounds.0.min(position);
            bounds.1 = bounds.1.max(position);
        }
    }
    SkyGeometry {
        origin: zone.origin,
        pan: zone.pan,
        scale: zone.scale,
        bounds,
        geometry: parts,
        bodies: bodies
            .into_iter()
            .filter(|body| body.location.distance(zone.origin) <= SKY_ZONE_RADIUS)
            .collect(),
    }
}

type EmitterActor = (String, Vec3, Option<ValueRange>, Option<i32>, Vec<String>);

fn emitter_actor(export: &ExportDump) -> EmitterActor {
    let location = property(&export.properties, "Location")
        .and_then(|property| triplet(&property.value))
        .map(unreal_direction_vec)
        .unwrap_or(Vec3::ZERO);
    // Lineage's VisibleLimit is the camera-distance window the effect plays
    // in. Emitter DrawScale is not part of it: most placed emitters carry a
    // DrawScale of 0.01, which would shrink their particles out of existence.
    let visible_limit = property(&export.properties, "VisibleLimit")
        .and_then(|property| serde_json::from_str::<serde_json::Value>(&property.value).ok())
        .map(|limit| {
            let field = |name: &str| {
                limit
                    .get(name)
                    .and_then(serde_json::Value::as_str)
                    .and_then(|value| value.parse::<f32>().ok())
            };
            ValueRange {
                min: field("Min").unwrap_or(0.0).max(0.0),
                max: field("Max").unwrap_or(f32::INFINITY),
            }
        });
    let references = property(&export.properties, "Emitters")
        .and_then(|property| serde_json::from_str(&property.value).ok())
        .unwrap_or_default();
    (
        export.name.clone(),
        location,
        visible_limit,
        game_event_id(&export.properties),
        references,
    )
}

// Lineage binds an emitter to a server-fired game event through L2GameEvent.
// Command one names the event that spawns the effect, so those actors -
// seasonal decorations, quest and boss effects - stay dormant until the viewer
// is asked to play them. The other commands belong to emitters the client
// plays from the moment the zone loads.
pub(super) fn game_event_id(properties: &[PropertyDump]) -> Option<i32> {
    let events = property(properties, "L2GameEvent").and_then(|property| {
        serde_json::from_str::<Vec<serde_json::Value>>(&property.value).ok()
    })?;
    events.iter().find_map(|event| {
        let field = |name: &str| {
            event
                .get(name)
                .and_then(serde_json::Value::as_str)?
                .parse::<i32>()
                .ok()
        };
        (field("EventCmd") == Some(1))
            .then(|| field("EventID"))
            .flatten()
    })
}

pub(super) fn scene_decal(export: &ExportDump) -> Option<SceneDecal> {
    let material = [
        "ProjTexture",
        "ProjectorMaterial",
        "DecalMaterial",
        "Material",
        "Texture",
    ]
    .into_iter()
    .find_map(|name| property(&export.properties, name))?
    .value
    .clone();
    let location = property(&export.properties, "Location")
        .and_then(|property| triplet(&property.value))
        .map(unreal_direction_vec)
        .unwrap_or(Vec3::ZERO);
    let rotation = property(&export.properties, "Rotation")
        .and_then(|property| triplet(&property.value))
        .unwrap_or(Vec3::ZERO);
    let draw_scale_3d = property(&export.properties, "DrawScale3D")
        .and_then(|property| triplet(&property.value))
        .unwrap_or(Vec3::ONE);
    let projection_depth = ["MaxTraceDistance", "ProjectorDistance", "Distance"]
        .into_iter()
        .find_map(|name| property(&export.properties, name))
        .and_then(|property| property.value.parse::<f32>().ok())
        .unwrap_or(256.0);
    let fov = property(&export.properties, "FOV")
        .and_then(|property| property.value.parse::<f32>().ok())
        .unwrap_or(90.0)
        .clamp(0.1, 179.0)
        .to_radians();
    let projection_width = 2.0 * projection_depth * (fov * 0.5).tan();
    Some(SceneDecal {
        name: export.name.clone(),
        material,
        location,
        rotation,
        scale: Vec3::new(
            draw_scale_3d.x * projection_width,
            draw_scale_3d.z * projection_width,
            projection_depth,
        ),
    })
}

pub(super) fn projector_transform(decal: &SceneDecal, origin: Vec3) -> Transform {
    let units_to_radians = std::f32::consts::TAU / 65_536.0;
    // Unreal projectors trace along local +X. Bevy clustered decals project
    // along local -Z and their unit cube is centered on the entity.
    let axis_correction = Quat::from_rotation_y(-std::f32::consts::FRAC_PI_2);
    let rotation = unreal_rotation(decal.rotation * units_to_radians) * axis_correction;
    let scale = decal.scale / UNREAL_UNITS_PER_METER;
    let direction = rotation * Vec3::NEG_Z;
    Transform {
        translation: (decal.location - origin) / UNREAL_UNITS_PER_METER + direction * scale.z * 0.5,
        rotation,
        scale,
    }
}

pub(super) fn property<'a>(properties: &'a [PropertyDump], name: &str) -> Option<&'a PropertyDump> {
    properties.iter().find(|value| value.name == name)
}

pub(super) fn flag(properties: &[PropertyDump], name: &str) -> bool {
    property(properties, name).is_some_and(|property| property.value.eq_ignore_ascii_case("true"))
}

#[cfg(test)]
pub(super) fn scene_environment(properties: &[PropertyDump]) -> SceneEnvironment {
    let mut environment = SceneEnvironment::default();
    merge_scene_environment(&mut environment, properties);
    environment
}

pub(super) fn merge_scene_environment(
    environment: &mut SceneEnvironment,
    properties: &[PropertyDump],
) {
    if let Some(ambient) =
        property(properties, "AmbientVector").and_then(|property| triplet(&property.value))
    {
        // ZoneInfo's AmbientVector is a cached, brightness-scaled color. The
        // authored AmbientBrightness is applied separately below, so using
        // the vector verbatim applies its magnitude twice (and, after sRGB
        // conversion, makes neutral outdoor zones almost black). Retain only
        // its channel ratios here; AmbientBrightness supplies the intensity.
        let peak = ambient.max_element();
        let ambient = if peak > f32::EPSILON {
            ambient / peak
        } else {
            Vec3::ONE
        };
        environment.ambient_color = Color::srgb(
            ambient.x.clamp(0.0, 1.0),
            ambient.y.clamp(0.0, 1.0),
            ambient.z.clamp(0.0, 1.0),
        );
    }
    if let Some(ambient_brightness) = property(properties, "AmbientBrightness")
        .and_then(|property| property.value.parse::<f32>().ok())
    {
        environment.ambient_brightness = ambient_brightness * 10.0;
    }
    let fog_enabled = flag(properties, "bDistanceFog");
    if fog_enabled {
        let color = property(properties, "DistanceFogColor")
            .and_then(property_color)
            .unwrap_or(Color::srgb(0.38, 0.38, 0.38));
        let start = property(properties, "DistanceFogStart")
            .and_then(|property| property.value.parse::<f32>().ok())
            .unwrap_or(0.0)
            / UNREAL_UNITS_PER_METER;
        let end = property(properties, "DistanceFogEnd")
            .and_then(|property| property.value.parse::<f32>().ok())
            .unwrap_or(65_536.0)
            / UNREAL_UNITS_PER_METER;
        environment.fog = Some(SceneFog {
            color,
            start,
            end: end.max(start + 1.0),
        });
    }
}

fn property_color(property: &PropertyDump) -> Option<Color> {
    let bytes = property
        .raw
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            std::str::from_utf8(pair)
                .ok()
                .and_then(|pair| u8::from_str_radix(pair, 16).ok())
        })
        .collect::<Option<Vec<_>>>()?;
    let [blue, green, red, alpha, ..] = bytes.as_slice() else {
        return None;
    };
    Some(Color::srgba_u8(*red, *green, *blue, *alpha))
}

fn triplet(value: &str) -> Option<Vec3> {
    let mut values = value.split(',').map(|value| value.trim().parse::<f32>());
    Some(Vec3::new(
        values.next()?.ok()?,
        values.next()?.ok()?,
        values.next()?.ok()?,
    ))
}

fn unreal_point(value: Vector3) -> [f32; 3] {
    [value.x, value.z, -value.y]
}

fn unreal_direction(value: &Vector3) -> Vec3 {
    unreal_direction_vec(Vec3::new(value.x, value.y, value.z)).normalize_or_zero()
}

pub(super) fn unreal_direction_vec(value: Vec3) -> Vec3 {
    Vec3::new(value.x, value.z, -value.y)
}

fn unreal_color(value: UnrealColor) -> [f32; 4] {
    [
        f32::from(value.red) / 255.0,
        f32::from(value.green) / 255.0,
        f32::from(value.blue) / 255.0,
        f32::from(value.alpha) / 255.0,
    ]
}
