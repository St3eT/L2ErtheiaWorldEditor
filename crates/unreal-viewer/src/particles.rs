use bevy::camera::primitives::{Aabb, Frustum};

use super::*;
use crate::scene::{
    flag, property, repeating_texture_image, resolve_texture, unreal_direction_vec,
};

// Unreal emitters are simulated on the CPU and drawn as one dynamic mesh per
// emitter: a sprite emitter needs per-particle colour, size, spin and sprite
// sheet cell, which a shared quad mesh cannot express.
const PARTICLE_DRAW_DISTANCE: f32 = 250.0;
const PARTICLE_POOL_LIMIT: usize = 400;
const UNREAL_DEFAULT_MAX_PARTICLES: usize = 10;
const UNREAL_DEFAULT_LIFETIME: f32 = 4.0;

pub(super) fn particle_definition(export: &ExportDump) -> ParticleSystemDefinition {
    let properties = &export.properties;
    let subdivisions = UVec2::new(
        integer(properties, "TextureUSubdivisions", 1).max(1),
        integer(properties, "TextureVSubdivisions", 1).max(1),
    );
    let cells = subdivisions.x * subdivisions.y;
    let subdivision_start = integer(properties, "SubdivisionStart", 0).min(cells - 1);
    let subdivision_end =
        integer(properties, "SubdivisionEnd", cells).clamp(subdivision_start + 1, cells);
    let lifetime = value_range(properties, "LifetimeRange", UNREAL_DEFAULT_LIFETIME);
    ParticleSystemDefinition {
        disabled: flag(properties, "Disabled"),
        texture: property(properties, "Texture")
            .filter(|property| !property.value.is_empty())
            .map(|property| property.value.clone()),
        blend: match number(properties, "DrawStyle", 0.0) as u8 {
            // PTDS_AlphaBlend and PTDS_AlphaModulate keep the sprite's own
            // alpha; PTDS_Modulated darkens what is behind it. Every other
            // style ends up additive in Lineage's renderer, which is also the
            // only sensible reading of the many opaque DXT1 particle sheets.
            1 | 4 => ParticleBlend::Alpha,
            2 => ParticleBlend::Modulate,
            _ => ParticleBlend::Add,
        },
        max_particles: integer(
            properties,
            "MaxParticles",
            UNREAL_DEFAULT_MAX_PARTICLES as u32,
        ) as usize,
        lifetime,
        start_shape: match number(properties, "StartLocationShape", 0.0) as u8 {
            1 => ParticleShape::Sphere,
            2 => ParticleShape::Polar,
            _ => ParticleShape::Box,
        },
        start_offset: unreal_direction_vec(vector(properties, "StartLocationOffset", Vec3::ZERO)),
        start_location: vector_range(properties, "StartLocationRange", 0.0).viewed(),
        start_polar: vector_range(properties, "StartLocationPolarRange", 0.0),
        sphere_radius: value_range(properties, "SphereRadiusRange", 0.0),
        start_velocity: vector_range(properties, "StartVelocityRange", 0.0).viewed(),
        velocity_direction: match number(properties, "GetVelocityDirectionFrom", 0.0) as u8 {
            1 => VelocityDirection::Outward,
            2 => VelocityDirection::Inward,
            _ => VelocityDirection::Authored,
        },
        acceleration: unreal_direction_vec(vector(properties, "Acceleration", Vec3::ZERO)),
        velocity_loss: vector_range(properties, "VelocityLossRange", 0.0).viewed(),
        start_size: vector_range(properties, "StartSizeRange", 1.0),
        uniform_size: flag(properties, "UniformSize"),
        size_scale: if flag(properties, "UseSizeScale") {
            curve(properties, "SizeScale", "RelativeSize")
        } else {
            Vec::new()
        },
        color_scale: if flag(properties, "UseColorScale") {
            color_curve(properties)
        } else {
            Vec::new()
        },
        color_scale_repeats: number(properties, "ColorScaleRepeats", 0.0).max(0.0),
        color_multiplier: vector_range(properties, "ColorMultiplierRange", 1.0),
        opacity: number(properties, "Opacity", 1.0),
        fade_in_end: if flag(properties, "FadeIn") {
            number(properties, "FadeInEndTime", 0.0)
        } else {
            0.0
        },
        fade_out_start: if flag(properties, "FadeOut") {
            number(properties, "FadeOutStartTime", lifetime.max)
        } else {
            f32::INFINITY
        },
        facing: match number(properties, "UseDirectionAs", 0.0) as u8 {
            1 => ParticleFacing::DirectionUp,
            4 | 5 => ParticleFacing::DirectionNormal,
            _ => ParticleFacing::Camera,
        },
        projection_normal: unreal_direction_vec(vector(properties, "ProjectionNormal", Vec3::Z)),
        spin: flag(properties, "SpinParticles"),
        start_spin: value_range_axis(properties, "StartSpinRange"),
        spins_per_second: value_range_axis(properties, "SpinsPerSecondRange"),
        counter_clockwise_chance: vector(properties, "SpinCCWorCW", Vec3::splat(0.5)).x,
        subdivisions,
        subdivision_start,
        subdivision_end,
        random_subdivision: flag(properties, "UseRandomSubdivision"),
        blend_subdivisions: flag(properties, "BlendBetweenSubdivisions"),
    }
}

pub(super) fn spawn_particle_systems(
    commands: &mut Commands,
    scene: &MapScene,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    images: &mut Assets<Image>,
) {
    let mut effect_images = HashMap::new();
    let mut effect_materials = HashMap::new();
    for effect in &scene.effects {
        let definition = &effect.definition;
        if definition.disabled || definition.max_particles == 0 {
            continue;
        }
        let Some(texture) = definition.texture.as_deref() else {
            continue;
        };
        let Some(resolved) = resolve_texture(scene, texture) else {
            continue;
        };
        let Some(geometry) = scene.textures.get(&resolved.texture) else {
            continue;
        };
        let image = effect_images
            .entry(resolved.texture.clone())
            .or_insert_with(|| images.add(masked_particle_image(geometry)))
            .clone();
        let blend = definition.blend;
        let material = effect_materials
            .entry((resolved.texture.clone(), blend))
            .or_insert_with(|| {
                materials.add(StandardMaterial {
                    base_color: Color::WHITE,
                    base_color_texture: Some(image.clone()),
                    alpha_mode: match blend {
                        ParticleBlend::Add => AlphaMode::Add,
                        ParticleBlend::Alpha => AlphaMode::Blend,
                        ParticleBlend::Modulate => AlphaMode::Multiply,
                    },
                    unlit: true,
                    double_sided: true,
                    cull_mode: None,
                    ..default()
                })
            })
            .clone();
        let emitter = ParticleEmitter::new(effect);
        // Filling the mesh here gives it the vertex layout and length it keeps
        // for the rest of its life, so the renderer allocates for it once.
        let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, selectable_mesh_usage());
        emitter.build_mesh(&mut mesh, &ParticleView::default());
        let emitter_bounds = emitter.bounds();
        commands.spawn((
            Name::new(effect.name.clone()),
            emitter,
            Mesh3d(meshes.add(mesh)),
            MeshMaterial3d(material),
            Transform::from_translation((effect.location - scene.origin) / UNREAL_UNITS_PER_METER),
            emitter_bounds,
            Visibility::Hidden,
            SelectableObject {
                actor: effect.name.clone(),
                object: "Engine.Emitter".into(),
                material: definition.texture.clone(),
                unreal_location: unreal_direction_vec(effect.location),
                export: None,
            },
            RayCastBackfaces,
        ));
    }
}

pub(super) fn simulate_particles(
    time: Res<Time>,
    events: Res<GameEventEffects>,
    camera: Query<(&GlobalTransform, &Frustum), With<FlyCamera>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut emitters: Query<(
        &mut ParticleEmitter,
        &Mesh3d,
        &GlobalTransform,
        &mut Aabb,
        &mut Visibility,
    )>,
) {
    let Ok((camera, frustum)) = camera.single() else {
        return;
    };
    // A long frame must not teleport particles through their whole life.
    let delta = time.delta_secs().min(0.1);
    let view = ParticleView {
        right: camera.right().into(),
        up: camera.up().into(),
        forward: camera.forward().into(),
    };
    for (mut emitter, mesh, transform, mut bounds, mut visibility) in &mut emitters {
        if !events.plays(emitter.game_event) {
            *visibility = Visibility::Hidden;
            continue;
        }
        let distance = transform.translation().distance(camera.translation());
        if distance < emitter.visible.min || distance > emitter.visible.max {
            *visibility = Visibility::Hidden;
            continue;
        }
        // Keep simulating out of sight so an effect is mid-flight when the
        // camera turns back to it, but only rebuild the mesh of an emitter the
        // camera can actually see.
        emitter.advance(delta);
        *bounds = emitter.bounds();
        if !frustum.intersects_obb(&bounds, &transform.affine(), true, false) {
            *visibility = Visibility::Hidden;
            continue;
        }
        let Some(mut mesh) = meshes.get_mut(&mesh.0) else {
            continue;
        };
        emitter.build_mesh(&mut mesh, &view);
        *visibility = Visibility::Visible;
    }
}

// Lineage's effect sheets are mostly DXT1, which carries no alpha: the sprite
// is drawn out of a black background, so its own brightness is the mask. Left
// opaque, the black around the sprite is added to whatever is behind the quad
// and prints the quad's outline onto the sky.
fn masked_particle_image(texture: &TextureGeometry) -> Image {
    if has_alpha_channel(texture) {
        return repeating_texture_image(texture);
    }
    repeating_texture_image(&TextureGeometry {
        width: texture.width,
        height: texture.height,
        rgba: texture
            .rgba
            .chunks_exact(4)
            .flat_map(|pixel| {
                [
                    pixel[0],
                    pixel[1],
                    pixel[2],
                    pixel[0].max(pixel[1]).max(pixel[2]),
                ]
            })
            .collect(),
    })
}

struct ParticleView {
    right: Vec3,
    up: Vec3,
    forward: Vec3,
}

impl Default for ParticleView {
    fn default() -> Self {
        Self {
            right: Vec3::X,
            up: Vec3::Y,
            forward: Vec3::NEG_Z,
        }
    }
}

#[derive(Component)]
pub(super) struct ParticleEmitter {
    definition: Arc<ParticleSystemDefinition>,
    visible: ValueRange,
    game_event: Option<i32>,
    particles: Vec<Particle>,
    rng: ParticleRng,
}

struct Particle {
    position: Vec3,
    velocity: Vec3,
    velocity_loss: Vec3,
    age: f32,
    lifetime: f32,
    size: Vec2,
    tint: Vec3,
    spin: f32,
    spin_rate: f32,
    cell: u32,
}

impl ParticleEmitter {
    // Unreal refills the pool as particles die, so a freshly loaded map starts
    // mid-flight rather than with every particle newly born.
    fn new(effect: &ParticleEffect) -> Self {
        let count = effect.definition.max_particles.min(PARTICLE_POOL_LIMIT);
        let visible = effect.visible_limit.unwrap_or(ValueRange {
            min: 0.0,
            max: f32::INFINITY,
        });
        let mut emitter = Self {
            particles: Vec::with_capacity(count),
            definition: effect.definition.clone(),
            game_event: effect.game_event,
            visible: ValueRange {
                min: visible.min / UNREAL_UNITS_PER_METER,
                max: (visible.max / UNREAL_UNITS_PER_METER).min(PARTICLE_DRAW_DISTANCE),
            },
            rng: ParticleRng::new(effect.name.as_str()),
        };
        for _ in 0..count {
            let mut particle = emitter.spawn();
            particle.age = emitter.rng.unit() * particle.lifetime;
            emitter.particles.push(particle);
        }
        emitter
    }

    fn advance(&mut self, delta: f32) {
        let acceleration = self.definition.acceleration;
        for index in 0..self.particles.len() {
            let particle = &mut self.particles[index];
            particle.age += delta;
            if particle.age >= particle.lifetime {
                self.particles[index] = self.spawn();
                continue;
            }
            particle.velocity += acceleration * delta;
            particle.velocity -= particle.velocity * particle.velocity_loss * delta;
            particle.position += particle.velocity * delta;
            particle.spin += particle.spin_rate * delta;
        }
    }

    fn spawn(&mut self) -> Particle {
        let definition = &self.definition;
        let offset = match definition.start_shape {
            ParticleShape::Box => self.rng.vector(definition.start_location),
            ParticleShape::Sphere => {
                self.rng.direction() * self.rng.range(definition.sphere_radius)
            }
            ParticleShape::Polar => {
                let polar = self.rng.vector(definition.start_polar);
                let (pitch, yaw) = (polar.x.to_radians(), polar.y.to_radians());
                unreal_direction_vec(Vec3::new(
                    pitch.cos() * yaw.cos(),
                    pitch.cos() * yaw.sin(),
                    pitch.sin(),
                )) * polar.z
            }
        };
        let position = definition.start_offset + offset;
        let velocity = self.rng.vector(definition.start_velocity);
        let velocity = match definition.velocity_direction {
            VelocityDirection::Authored => velocity,
            VelocityDirection::Outward => position.normalize_or_zero() * velocity.length(),
            VelocityDirection::Inward => -position.normalize_or_zero() * velocity.length(),
        };
        let size = self.rng.vector(definition.start_size);
        let size = if definition.uniform_size {
            Vec2::splat(size.x)
        } else {
            size.truncate()
        };
        let spin_direction = if self.rng.unit() < definition.counter_clockwise_chance {
            1.0
        } else {
            -1.0
        };
        let cells = definition.subdivision_end - definition.subdivision_start;
        Particle {
            position,
            velocity,
            velocity_loss: self.rng.vector(definition.velocity_loss),
            age: 0.0,
            lifetime: self.rng.range(definition.lifetime).max(0.01),
            size,
            tint: self.rng.vector(definition.color_multiplier),
            spin: if definition.spin {
                self.rng.range(definition.start_spin) * std::f32::consts::TAU
            } else {
                0.0
            },
            spin_rate: if definition.spin {
                self.rng.range(definition.spins_per_second) * std::f32::consts::TAU * spin_direction
            } else {
                0.0
            },
            cell: definition.subdivision_start + (self.rng.unit() * cells as f32) as u32 % cells,
        }
    }

    /// A conservative local-space bound, cheap enough to keep current for an
    /// emitter whose mesh the camera cannot see.
    fn bounds(&self) -> Aabb {
        let mut minimum = Vec3::splat(f32::INFINITY);
        let mut maximum = Vec3::splat(f32::NEG_INFINITY);
        for particle in &self.particles {
            let relative = (particle.age / particle.lifetime).clamp(0.0, 1.0);
            let reach = (particle.size * self.definition.size_at(relative) * 0.5
                / UNREAL_UNITS_PER_METER)
                .max_element();
            let center = particle.position / UNREAL_UNITS_PER_METER;
            minimum = minimum.min(center - reach);
            maximum = maximum.max(center + reach);
        }
        if minimum.is_finite() && maximum.is_finite() {
            Aabb::from_min_max(minimum, maximum)
        } else {
            Aabb::default()
        }
    }

    fn build_mesh(&self, mesh: &mut Mesh, view: &ParticleView) {
        let definition = &self.definition;
        let quads = self.particles.len() * if definition.blend_subdivisions { 2 } else { 1 };
        let mut positions = Vec::with_capacity(quads * 4);
        let mut normals = Vec::with_capacity(quads * 4);
        let mut uvs = Vec::with_capacity(quads * 4);
        let mut colors = Vec::with_capacity(quads * 4);
        let mut indices = Vec::with_capacity(quads * 6);
        for particle in &self.particles {
            let relative = (particle.age / particle.lifetime).clamp(0.0, 1.0);
            let center = particle.position / UNREAL_UNITS_PER_METER;
            let extent =
                particle.size * definition.size_at(relative) * 0.5 / UNREAL_UNITS_PER_METER;
            let (right, up) = definition.axes(particle, view);
            let (sine, cosine) = particle.spin.sin_cos();
            let (right, up) = (
                (right * cosine + up * sine) * extent.x,
                (up * cosine - right * sine) * extent.y,
            );
            let color = definition.color_at(particle, relative);
            let (cell, next, blend) = definition.cell_at(particle, relative);
            // Blended subdivisions always emit both cells, even at zero
            // weight, so the mesh keeps a constant length between frames.
            let cells = [(cell, 1.0 - blend), (next, blend)];
            for &(cell, weight) in &cells[..if definition.blend_subdivisions { 2 } else { 1 }] {
                let corners = [
                    center - right - up,
                    center + right - up,
                    center + right + up,
                    center - right + up,
                ];
                let base = positions.len() as u32;
                let (u0, v0, u1, v1) = definition.cell_uv(cell);
                for (corner, uv) in
                    corners
                        .into_iter()
                        .zip([[u0, v1], [u1, v1], [u1, v0], [u0, v0]])
                {
                    positions.push(corner.to_array());
                    normals.push((-view.forward).to_array());
                    uvs.push(uv);
                    colors.push([color.red, color.green, color.blue, color.alpha * weight]);
                }
                indices.extend([base, base + 1, base + 2, base, base + 2, base + 3]);
            }
        }
        mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
        mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
        mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
        mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, colors);
        mesh.insert_indices(Indices::U32(indices));
    }
}

impl ParticleSystemDefinition {
    fn size_at(&self, relative: f32) -> f32 {
        curve_at(&self.size_scale, relative).unwrap_or(1.0)
    }

    fn color_at(&self, particle: &Particle, relative: f32) -> LinearRgba {
        let repeats = self.color_scale_repeats;
        let scaled = if repeats > 0.0 {
            (relative * (repeats + 1.0)).fract()
        } else {
            relative
        };
        let color = color_at(&self.color_scale, scaled).unwrap_or(LinearRgba::WHITE);
        let mut alpha = self.opacity * color.alpha;
        if particle.age < self.fade_in_end {
            alpha *= particle.age / self.fade_in_end;
        }
        if particle.age > self.fade_out_start {
            let span = particle.lifetime - self.fade_out_start;
            alpha *= if span > 0.0 {
                ((particle.lifetime - particle.age) / span).clamp(0.0, 1.0)
            } else {
                0.0
            };
        }
        LinearRgba::new(
            color.red * particle.tint.x,
            color.green * particle.tint.y,
            color.blue * particle.tint.z,
            alpha.clamp(0.0, 1.0),
        )
    }

    fn axes(&self, particle: &Particle, view: &ParticleView) -> (Vec3, Vec3) {
        match self.facing {
            ParticleFacing::Camera => (view.right, view.up),
            ParticleFacing::DirectionUp => {
                let up = particle.velocity.normalize_or(Vec3::Y);
                let right = up.cross(view.forward).normalize_or(view.right);
                (right, up)
            }
            ParticleFacing::DirectionNormal => {
                let normal = particle
                    .velocity
                    .normalize_or(self.projection_normal.normalize_or(Vec3::Y));
                let right = normal.cross(Vec3::Y).normalize_or(Vec3::X);
                (right, normal.cross(right))
            }
        }
    }

    fn cell_at(&self, particle: &Particle, relative: f32) -> (u32, u32, f32) {
        if self.random_subdivision {
            return (particle.cell, particle.cell, 0.0);
        }
        let span = self.subdivision_end - self.subdivision_start;
        let progress = relative * span as f32;
        let cell = (progress as u32).min(span - 1);
        let next = (cell + 1).min(span - 1);
        (
            self.subdivision_start + cell,
            self.subdivision_start + next,
            if self.blend_subdivisions {
                progress.fract()
            } else {
                0.0
            },
        )
    }

    fn cell_uv(&self, cell: u32) -> (f32, f32, f32, f32) {
        let columns = self.subdivisions.x;
        let (column, row) = (cell % columns, cell / columns);
        let size = Vec2::new(1.0 / columns as f32, 1.0 / self.subdivisions.y as f32);
        (
            column as f32 * size.x,
            row as f32 * size.y,
            (column + 1) as f32 * size.x,
            (row + 1) as f32 * size.y,
        )
    }
}

impl VectorRange {
    fn viewed(self) -> Self {
        let first = unreal_direction_vec(self.min);
        let second = unreal_direction_vec(self.max);
        Self {
            min: first.min(second),
            max: first.max(second),
        }
    }
}

struct ParticleRng(u32);

impl ParticleRng {
    fn new(seed: &str) -> Self {
        let mut state = 0x811c_9dc5_u32;
        for byte in seed.as_bytes() {
            state = (state ^ u32::from(*byte)).wrapping_mul(0x0100_0193);
        }
        Self(state | 1)
    }

    fn unit(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        (self.0 >> 8) as f32 / 16_777_216.0
    }

    fn range(&mut self, range: ValueRange) -> f32 {
        range.min + (range.max - range.min) * self.unit()
    }

    fn vector(&mut self, range: VectorRange) -> Vec3 {
        let unit = Vec3::new(self.unit(), self.unit(), self.unit());
        range.min + (range.max - range.min) * unit
    }

    fn direction(&mut self) -> Vec3 {
        let z = self.unit() * 2.0 - 1.0;
        let angle = self.unit() * std::f32::consts::TAU;
        let radius = (1.0 - z * z).max(0.0).sqrt();
        Vec3::new(radius * angle.cos(), z, radius * angle.sin())
    }
}

fn number(properties: &[PropertyDump], name: &str, default: f32) -> f32 {
    property(properties, name)
        .and_then(|property| property.value.parse().ok())
        .unwrap_or(default)
}

fn integer(properties: &[PropertyDump], name: &str, default: u32) -> u32 {
    property(properties, name)
        .and_then(|property| property.value.parse::<i64>().ok())
        .and_then(|value| u32::try_from(value).ok())
        .unwrap_or(default)
}

fn vector(properties: &[PropertyDump], name: &str, default: Vec3) -> Vec3 {
    property(properties, name)
        .and_then(|property| structured_triplet(&property.value))
        .unwrap_or(default)
}

fn structured_triplet(value: &str) -> Option<Vec3> {
    let mut values = value.split(',').map(|value| value.trim().parse::<f32>());
    Some(Vec3::new(
        values.next()?.ok()?,
        values.next()?.ok()?,
        values.next()?.ok()?,
    ))
}

fn value_range(properties: &[PropertyDump], name: &str, default: f32) -> ValueRange {
    property(properties, name)
        .and_then(|property| serde_json::from_str::<serde_json::Value>(&property.value).ok())
        .map(|value| range_fields(&value, default))
        .unwrap_or(ValueRange {
            min: default,
            max: default,
        })
}

// Sprites roll around a single axis, which Unreal stores in a range vector's X.
fn value_range_axis(properties: &[PropertyDump], name: &str) -> ValueRange {
    property(properties, name)
        .and_then(|property| serde_json::from_str::<serde_json::Value>(&property.value).ok())
        .and_then(|value| value.get("X").map(|axis| range_fields(axis, 0.0)))
        .unwrap_or(ValueRange { min: 0.0, max: 0.0 })
}

fn vector_range(properties: &[PropertyDump], name: &str, default: f32) -> VectorRange {
    let Some(value) = property(properties, name)
        .and_then(|property| serde_json::from_str::<serde_json::Value>(&property.value).ok())
    else {
        return VectorRange {
            min: Vec3::splat(default),
            max: Vec3::splat(default),
        };
    };
    let axis = |name: &str| {
        value
            .get(name)
            .map(|axis| range_fields(axis, default))
            .unwrap_or(ValueRange {
                min: default,
                max: default,
            })
    };
    let (x, y, z) = (axis("X"), axis("Y"), axis("Z"));
    VectorRange {
        min: Vec3::new(x.min, y.min, z.min),
        max: Vec3::new(x.max, y.max, z.max),
    }
}

fn range_fields(value: &serde_json::Value, default: f32) -> ValueRange {
    let field = |name: &str| {
        value
            .get(name)
            .and_then(serde_json::Value::as_str)
            .and_then(|value| value.parse::<f32>().ok())
    };
    ValueRange {
        min: field("Min").unwrap_or(default),
        max: field("Max").unwrap_or(default),
    }
}

fn curve(properties: &[PropertyDump], name: &str, field: &str) -> Vec<(f32, f32)> {
    curve_points(properties, name)
        .into_iter()
        .filter_map(|(time, point)| {
            let value = point
                .get(field)
                .and_then(serde_json::Value::as_str)?
                .parse()
                .ok()?;
            Some((time, value))
        })
        .collect()
}

fn color_curve(properties: &[PropertyDump]) -> Vec<(f32, LinearRgba)> {
    curve_points(properties, "ColorScale")
        .into_iter()
        .filter_map(|(time, point)| {
            let value = point.get("Color").and_then(serde_json::Value::as_str)?;
            let mut channels = value.split(',').map(|channel| channel.trim().parse::<u8>());
            let color = Color::srgba_u8(
                channels.next()?.ok()?,
                channels.next()?.ok()?,
                channels.next()?.ok()?,
                channels.next().and_then(Result::ok).unwrap_or(u8::MAX),
            );
            Some((time, color.to_linear()))
        })
        .collect()
}

fn curve_points(properties: &[PropertyDump], name: &str) -> Vec<(f32, serde_json::Value)> {
    let Some(points) = property(properties, name)
        .and_then(|property| serde_json::from_str::<Vec<serde_json::Value>>(&property.value).ok())
    else {
        return Vec::new();
    };
    let mut points: Vec<_> = points
        .into_iter()
        .filter_map(|point| {
            let time: f32 = point
                .get("RelativeTime")
                .and_then(serde_json::Value::as_str)?
                .parse()
                .ok()?;
            Some((time, point))
        })
        .collect();
    points.sort_by(|(left, _), (right, _)| left.total_cmp(right));
    points
}

fn curve_at(points: &[(f32, f32)], time: f32) -> Option<f32> {
    let (left, right, fraction) = curve_segment(points, time)?;
    Some(points[left].1 + (points[right].1 - points[left].1) * fraction)
}

fn color_at(points: &[(f32, LinearRgba)], time: f32) -> Option<LinearRgba> {
    let (left, right, fraction) = curve_segment(points, time)?;
    Some(points[left].1.mix(&points[right].1, fraction))
}

fn curve_segment<T>(points: &[(f32, T)], time: f32) -> Option<(usize, usize, f32)> {
    if points.is_empty() {
        return None;
    }
    let right = points.iter().position(|(point, _)| *point >= time);
    match right {
        None => Some((points.len() - 1, points.len() - 1, 0.0)),
        Some(0) => Some((0, 0, 0.0)),
        Some(right) => {
            let span = points[right].0 - points[right - 1].0;
            let fraction = if span > 0.0 {
                (time - points[right - 1].0) / span
            } else {
                0.0
            };
            Some((right - 1, right, fraction))
        }
    }
}

#[cfg(test)]
mod tests {
    use bevy::math::Vec3A;

    use super::*;

    fn property(name: &str, value: &str) -> PropertyDump {
        PropertyDump {
            name: name.into(),
            property_type: String::new(),
            array_index: 0,
            size: 0,
            value: value.into(),
            raw: String::new(),
            wire_header: String::new(),
        }
    }

    fn sprite_emitter(properties: Vec<PropertyDump>) -> ParticleSystemDefinition {
        particle_definition(&ExportDump {
            index: 0,
            reference: 1,
            name: "SpriteEmitter187".into(),
            full_name: "SpriteEmitter187".into(),
            class: "Engine.SpriteEmitter".into(),
            super_class: String::new(),
            outer: 0,
            flags: String::new(),
            size: 0,
            offset: 0,
            properties,
            stack: None,
            native_object: None,
            unread_data: String::new(),
            raw_data: None,
        })
    }

    #[test]
    fn sprite_emitter_reads_its_unreal_sized_particles() {
        let definition = sprite_emitter(vec![
            property("MaxParticles", "17"),
            property("UniformSize", "true"),
            property(
                "StartSizeRange",
                r#"{"X":{"Max":"60","Min":"24"},"Y":{"Max":"60","Min":"24"}}"#,
            ),
            property("LifetimeRange", r#"{"Max":"0.8","Min":"0.8"}"#),
            property("FadeOut", "true"),
            property("FadeOutStartTime", "0.3"),
            property("Opacity", "0.79"),
        ]);

        assert_eq!(definition.max_particles, 17);
        assert!(definition.uniform_size);
        assert_eq!(definition.start_size.min.x, 24.0);
        assert_eq!(definition.start_size.max.x, 60.0);
        assert_eq!(definition.lifetime.max, 0.8);
        assert_eq!(definition.fade_out_start, 0.3);
        assert_eq!(definition.opacity, 0.79);
    }

    #[test]
    fn an_unset_flag_keeps_its_authored_scale_curve_unused() {
        let scales =
            r#"[{"RelativeSize":"1","RelativeTime":"0"},{"RelativeSize":"2","RelativeTime":"1"}]"#;
        let unused = sprite_emitter(vec![property("SizeScale", scales)]);
        let used = sprite_emitter(vec![
            property("UseSizeScale", "true"),
            property("SizeScale", scales),
        ]);

        assert_eq!(unused.size_at(1.0), 1.0);
        assert_eq!(used.size_at(0.5), 1.5);
    }

    #[test]
    fn subdivisions_default_to_the_whole_sprite_sheet() {
        let definition = sprite_emitter(vec![
            property("TextureUSubdivisions", "8"),
            property("TextureVSubdivisions", "4"),
        ]);

        assert_eq!(definition.subdivision_start, 0);
        assert_eq!(definition.subdivision_end, 32);
        assert_eq!(definition.cell_uv(9), (0.125, 0.25, 0.25, 0.5));
    }

    #[test]
    fn a_sprite_sheet_plays_across_a_particle_lifetime() {
        let definition = sprite_emitter(vec![
            property("TextureUSubdivisions", "2"),
            property("TextureVSubdivisions", "2"),
        ]);
        let particle = test_particle(0);

        assert_eq!(definition.cell_at(&particle, 0.0).0, 0);
        assert_eq!(definition.cell_at(&particle, 0.6).0, 2);
        assert_eq!(definition.cell_at(&particle, 1.0).0, 3);
    }

    #[test]
    fn a_random_subdivision_holds_the_cell_the_particle_was_born_with() {
        let definition = sprite_emitter(vec![
            property("TextureUSubdivisions", "2"),
            property("TextureVSubdivisions", "2"),
            property("UseRandomSubdivision", "true"),
            property("SubdivisionStart", "3"),
            property("SubdivisionEnd", "4"),
        ]);

        assert_eq!(definition.subdivision_start, 3);
        assert_eq!(definition.cell_at(&test_particle(3), 0.5), (3, 3, 0.0));
    }

    #[test]
    fn only_a_spawn_command_binds_an_emitter_to_a_game_event() {
        let event = |command: &str| {
            property(
                "L2GameEvent",
                &format!(r#"[{{"EventCmd":"{command}","EventID":"24180446"}}]"#),
            )
        };

        assert_eq!(crate::scene::game_event_id(&[event("1")]), Some(24180446));
        assert_eq!(crate::scene::game_event_id(&[event("4")]), None);
        assert_eq!(crate::scene::game_event_id(&[]), None);
    }

    #[test]
    fn fades_scale_a_particle_towards_transparency() {
        let definition = sprite_emitter(vec![
            property("FadeIn", "true"),
            property("FadeInEndTime", "0.2"),
            property("FadeOut", "true"),
            property("FadeOutStartTime", "0.6"),
        ]);
        let faded = |age| {
            let mut particle = test_particle(0);
            particle.age = age;
            definition.color_at(&particle, age).alpha
        };

        assert!((faded(0.1) - 0.5).abs() < 1.0e-6);
        assert_eq!(faded(0.4), 1.0);
        assert!((faded(0.8) - 0.5).abs() < 1.0e-6);
    }

    #[test]
    fn emitter_bounds_cover_every_particle_it_spawned() {
        let definition = sprite_emitter(vec![
            property("MaxParticles", "24"),
            property(
                "StartLocationRange",
                r#"{"X":{"Max":"300","Min":"-300"},"Y":{"Max":"300","Min":"-300"},"Z":{"Max":"100","Min":"-100"}}"#,
            ),
            property(
                "StartSizeRange",
                r#"{"X":{"Max":"64","Min":"64"},"Y":{"Max":"64","Min":"64"}}"#,
            ),
        ]);
        let emitter = ParticleEmitter::new(&ParticleEffect {
            name: "Emitter0.SpriteEmitter0".into(),
            location: Vec3::ZERO,
            visible_limit: None,
            game_event: None,
            definition: Arc::new(definition),
        });
        let bounds = emitter.bounds();

        assert_eq!(emitter.particles.len(), 24);
        for particle in &emitter.particles {
            let center = Vec3A::from(particle.position / UNREAL_UNITS_PER_METER);
            assert!(
                (center - bounds.center)
                    .abs()
                    .cmple(bounds.half_extents)
                    .all()
            );
        }
    }

    fn test_particle(cell: u32) -> Particle {
        Particle {
            position: Vec3::ZERO,
            velocity: Vec3::ZERO,
            velocity_loss: Vec3::ZERO,
            age: 0.0,
            lifetime: 1.0,
            size: Vec2::ONE,
            tint: Vec3::ONE,
            spin: 0.0,
            spin_rate: 0.0,
            cell,
        }
    }
}
