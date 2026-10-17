use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use bevy::asset::RenderAssetUsages;
use bevy::camera::visibility::RenderLayers;
use bevy::ecs::system::SystemParam;
use bevy::image::{ImageAddressMode, ImageSampler, ImageSamplerDescriptor};
use bevy::input::mouse::{AccumulatedMouseMotion, AccumulatedMouseScroll};
use bevy::light::{ClusteredDecal, cluster::GlobalClusterSettings};
use bevy::mesh::{
    Indices, VertexAttributeValues,
    skinning::{SkinnedMesh, SkinnedMeshInverseBindposes},
};
use bevy::picking::mesh_picking::ray_cast::{MeshRayCast, MeshRayCastSettings, RayCastBackfaces};
use bevy::prelude::*;
use bevy::render::RenderApp;
use bevy::render::render_resource::{
    Extent3d, Face, PrimitiveTopology, TextureDimension, TextureFormat,
};
use bevy::ui::RelativeCursorPosition;

use unreal_package::codec::BinaryReader;
use unreal_package::native::{
    Color as UnrealColor, Coordinates, MeshAnimation, NativeObject, SkeletalMesh, StaticMesh,
    Vector3,
};
use unreal_package::unreal::{ExportDump, Package, PropertyDump};

mod combiner;
mod particles;
mod presentation;
mod runtime;
mod scene;
mod terrain;

use presentation::ViewerPresentationPlugin;
use runtime::ViewerRuntimePlugin;
use scene::{asset_roots, texture_geometry};

const UNREAL_UNITS_PER_METER: f32 = 128.0;
const RADAR_PAGE_SIZE: usize = 1024;
const RADAR_COLUMNS: usize = 5;
const RADAR_ROWS: usize = 4;
const RADAR_TILE_SIZE: f32 = 246.0;
type BlockDecoder = fn(&[u8], &mut [u8], usize);

fn selectable_mesh_usage() -> RenderAssetUsages {
    RenderAssetUsages::MAIN_WORLD | RenderAssetUsages::RENDER_WORLD
}

pub fn run(path: &Path, asset_paths: &[PathBuf]) -> Result<(), String> {
    if let Some(next) = run_map(path, asset_paths)? {
        let executable = std::env::current_exe()
            .map_err(|error| format!("cannot locate viewer executable: {error}"))?;
        let mut command = std::process::Command::new(executable);
        command.args(["unr", "view", "--input"]).arg(next);
        for asset_path in asset_paths {
            command.arg("--assets").arg(asset_path);
        }
        command
            .spawn()
            .map_err(|error| format!("cannot open selected map: {error}"))?;
    }
    Ok(())
}

fn run_map(path: &Path, asset_paths: &[PathBuf]) -> Result<Option<PathBuf>, String> {
    let (scene, load_report) = load_scene(path, asset_paths)?;
    report_scene(&scene, &load_report);
    let maps_dir = path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    let radar_path = maps_dir
        .parent()
        .map(|root| root.join("SysTextures/L2_RadarMap.utx"));
    let radar =
        radar_path
            .filter(|path| path.is_file())
            .and_then(|path| match load_radar_atlas(&path) {
                Ok(atlas) => Some(atlas),
                Err(error) => {
                    eprintln!("viewer: warning: cannot load {}: {error}", path.display());
                    None
                }
            });
    let next_map = Arc::new(Mutex::new(None));
    let mut app = App::new();
    app.insert_resource(scene)
        .insert_resource(MapNavigation {
            maps_dir,
            radar,
            next: next_map.clone(),
        })
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: format!("unreal-rs — {}", path.display()),
                ..default()
            }),
            ..default()
        }))
        .add_plugins(LargeSceneClusterPlugin)
        .add_plugins(combiner::CombinerPlugin)
        .add_plugins(terrain::TerrainPlugin)
        .add_plugins(ViewerPresentationPlugin)
        .add_plugins(ViewerRuntimePlugin)
        .run();
    let selected = take_next_map(&next_map)?;
    Ok(selected)
}

fn store_next_map(next: &Mutex<Option<PathBuf>>, path: PathBuf) -> Result<(), String> {
    *next
        .lock()
        .map_err(|_| "selected-map handoff was poisoned")? = Some(path);
    Ok(())
}

fn take_next_map(next: &Mutex<Option<PathBuf>>) -> Result<Option<PathBuf>, String> {
    Ok(next
        .lock()
        .map_err(|_| "selected-map handoff was poisoned")?
        .take())
}

struct LargeSceneClusterPlugin;

impl Plugin for LargeSceneClusterPlugin {
    fn build(&self, _app: &mut App) {}

    fn finish(&self, app: &mut App) {
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };
        let Some(mut settings) = render_app
            .world_mut()
            .get_resource_mut::<GlobalClusterSettings>()
        else {
            return;
        };
        if let Some(gpu_clustering) = &mut settings.gpu_clustering {
            gpu_clustering.initial_z_slice_list_capacity =
                gpu_clustering.initial_z_slice_list_capacity.max(4096);
        }
    }
}

fn load_scene(
    path: &Path,
    asset_paths: &[PathBuf],
) -> Result<(MapScene, ExternalLoadReport), String> {
    let package = Package::read(path)?;
    let dump = package.dump();
    let object_paths = dump
        .imports
        .iter()
        .map(|import| (import.reference, import.full_name.clone()))
        .chain(
            dump.exports
                .iter()
                .map(|export| (export.reference, export.full_name.clone())),
        )
        .collect();
    let mut scene = MapScene::from_exports(dump.exports, object_paths)?;
    let asset_roots = asset_roots(path, asset_paths);
    let load_report = scene.load_external_meshes(&asset_roots)?;
    Ok((scene, load_report))
}

fn report_scene(scene: &MapScene, load_report: &ExternalLoadReport) {
    for warning in &load_report.warnings {
        eprintln!("viewer: warning: {warning}");
    }
    eprintln!(
        "viewer: {} terrain vertex(es), {} BSP triangle(s), {} embedded mesh(es), {} external mesh package(s), {} material package(s), {} texture(s), {} placed mesh/mover actor(s), {} light(s), {} emitter component(s), {} unresolved mesh actor(s)",
        scene
            .terrain
            .as_ref()
            .map_or(0, |terrain| terrain.positions.len()),
        scene
            .bsp
            .iter()
            .flat_map(|geometry| &geometry.parts)
            .map(|part| part.indices.len() / 3)
            .sum::<usize>(),
        scene.embedded_meshes,
        load_report.loaded_packages,
        load_report.loaded_material_packages,
        load_report.loaded_textures,
        scene.resolved_instance_count(),
        scene.lights.len(),
        scene.effects.len(),
        scene.unresolved_instance_count(),
    );
    if let Some(sky) = &scene.sky {
        eprintln!(
            "viewer: sky zone with {} triangle(s) and {} sky bod{}",
            sky.geometry
                .iter()
                .flat_map(|geometry| &geometry.parts)
                .map(|part| part.indices.len() / 3)
                .sum::<usize>(),
            sky.bodies.len(),
            if sky.bodies.len() == 1 { "y" } else { "ies" },
        );
    }
    let events = scene.game_event_counts();
    if !events.is_empty() {
        eprintln!(
            "viewer: {} emitter component(s) across {} game event(s) are dormant; press G to play them",
            events.values().sum::<usize>(),
            events.len(),
        );
    }
}

fn load_radar_atlas(path: &Path) -> Result<RadarAtlas, String> {
    let exports = Package::read(path)?.dump().exports;
    let mut pages = BTreeMap::new();
    for export in exports {
        let Some(page) = radar_page_number(&export.name) else {
            continue;
        };
        let Some(NativeObject::Texture(texture)) = export.native_object else {
            continue;
        };
        let Some(decoded) = texture_geometry(&export.properties, texture) else {
            continue;
        };
        if decoded.width as usize != RADAR_PAGE_SIZE || decoded.height as usize != RADAR_PAGE_SIZE {
            return Err(format!(
                "{} is {}x{}, expected {RADAR_PAGE_SIZE}x{RADAR_PAGE_SIZE}",
                export.name, decoded.width, decoded.height
            ));
        }
        pages.insert(page, decoded.rgba);
    }
    if pages.len() != RADAR_COLUMNS * RADAR_ROWS {
        return Err(format!(
            "found {} radar pages, expected {}",
            pages.len(),
            RADAR_COLUMNS * RADAR_ROWS
        ));
    }
    let width = RADAR_PAGE_SIZE * RADAR_COLUMNS;
    let height = RADAR_PAGE_SIZE * RADAR_ROWS;
    let mut rgba = vec![0; width * height * 4];
    for page in 1..=RADAR_COLUMNS * RADAR_ROWS {
        let source = pages
            .get(&page)
            .ok_or_else(|| format!("RadarMap_{page} is missing"))?;
        let page_x = (page - 1) % RADAR_COLUMNS;
        let page_y = (page - 1) / RADAR_COLUMNS;
        for y in 0..RADAR_PAGE_SIZE {
            let source_start = y * RADAR_PAGE_SIZE * 4;
            let target_start =
                ((page_y * RADAR_PAGE_SIZE + y) * width + page_x * RADAR_PAGE_SIZE) * 4;
            rgba[target_start..target_start + RADAR_PAGE_SIZE * 4]
                .copy_from_slice(&source[source_start..source_start + RADAR_PAGE_SIZE * 4]);
        }
    }
    Ok(RadarAtlas { rgba })
}

fn radar_page_number(name: &str) -> Option<usize> {
    name.to_ascii_lowercase()
        .strip_prefix("radarmap_")?
        .parse()
        .ok()
}

fn radar_map_name(relative_cursor: Vec2) -> String {
    let normalized = relative_cursor + Vec2::splat(0.5);
    let pixel_x = normalized.x.clamp(0.0, 0.999_999) * (RADAR_PAGE_SIZE * RADAR_COLUMNS) as f32;
    let pixel_y = normalized.y.clamp(0.0, 0.999_999) * (RADAR_PAGE_SIZE * RADAR_ROWS) as f32;
    format!(
        "{}_{}.unr",
        10 + (pixel_x / RADAR_TILE_SIZE).floor() as usize,
        10 + (pixel_y / RADAR_TILE_SIZE).floor() as usize
    )
}

#[derive(Resource)]
struct MapScene {
    origin: Vec3,
    view_center: Vec3,
    view_radius: f32,
    terrain: Option<TerrainGeometry>,
    pending_terrain: Option<PendingTerrain>,
    meshes: HashMap<String, Arc<MeshGeometry>>,
    animations: HashMap<String, Arc<AnimationGeometry>>,
    materials: HashMap<String, MaterialGeometry>,
    textures: HashMap<String, Arc<TextureGeometry>>,
    instances: Vec<MeshInstance>,
    lights: Vec<SceneLight>,
    effects: Vec<ParticleEffect>,
    decals: Vec<SceneDecal>,
    player_starts: Vec<PlayerStart>,
    bsp: Vec<MeshGeometry>,
    environment: SceneEnvironment,
    embedded_meshes: usize,
    sky: Option<SkyGeometry>,
}

#[derive(Clone, Copy)]
struct SkyZone {
    origin: Vec3,
    pan: Vec2,
    scale: f32,
}

/// The level geometry that belongs to the sky zone, in world position.
struct SkyGeometry {
    origin: Vec3,
    pan: Vec2,
    scale: f32,
    /// How far the sky room reaches from its zone actor, per axis.
    bounds: (Vec3, Vec3),
    geometry: Vec<MeshGeometry>,
    /// The sun and moon Lineage hangs in the sky room.
    bodies: Vec<SkyBody>,
}

struct SkyBody {
    name: String,
    /// The moon stands in for the sun once the hour turns.
    night: bool,
    texture: String,
    location: Vec3,
    radius: f32,
}

#[derive(Component)]
struct SkyCamera;

#[derive(Clone)]
struct PlayerStart {
    name: String,
    location: Vec3,
}

struct SceneEnvironment {
    ambient_color: Color,
    ambient_brightness: f32,
    fog: Option<SceneFog>,
}

#[derive(Clone)]
struct SceneFog {
    color: Color,
    start: f32,
    end: f32,
}

#[derive(Default)]
struct ExternalLoadReport {
    loaded_packages: usize,
    loaded_material_packages: usize,
    loaded_textures: usize,
    warnings: Vec<String>,
}

struct TerrainGeometry {
    width: usize,
    height: usize,
    positions: Vec<[f32; 3]>,
    colors: Vec<[f32; 4]>,
    layers: TerrainLayering,
    quad_visibility: Vec<u8>,
    edge_turns: Vec<u8>,
}

/// How a terrain is painted: its layers in drawing order and how large one
/// of its quads is in world units.
#[derive(Clone, Debug, Default, PartialEq)]
struct TerrainLayering {
    quad_size: Vec2,
    layers: Vec<TerrainLayer>,
}

#[derive(Clone, Debug, PartialEq)]
struct TerrainLayer {
    texture: String,
    alpha_map: Option<String>,
    /// UScale and VScale: world units per texel of the layer texture.
    scale: Vec2,
}

struct PendingTerrain {
    heightmap_texture: String,
    width: usize,
    height: usize,
    to_world: Coordinates,
    colors: Vec<UnrealColor>,
    layers: TerrainLayering,
    topology: TerrainTopology,
}

#[derive(Default)]
struct TerrainTopology {
    quad_visibility: Vec<u8>,
    edge_turns: Vec<u8>,
}

struct MeshGeometry {
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    uvs: Vec<[f32; 2]>,
    /// The alpha painted onto a static mesh's vertices, which combiners
    /// masked by an Engine.VertexColor blend their two materials by.
    vertex_alpha: Option<Vec<f32>>,
    parts: Vec<MeshPart>,
    skin: Option<SkinGeometry>,
}

struct SkinGeometry {
    bones: Vec<BoneGeometry>,
    joint_indices: Vec<[u16; 4]>,
    joint_weights: Vec<[f32; 4]>,
    animation: String,
}

struct BoneGeometry {
    name: String,
    parent: Option<usize>,
    transform: Transform,
}

struct AnimationGeometry {
    duration: f32,
    tracks: Vec<Option<AnimationTrackGeometry>>,
}

struct AnimationTrackGeometry {
    times: Vec<f32>,
    rotations: Vec<Quat>,
    translations: Vec<Vec3>,
}

#[derive(Default)]
struct MeshPart {
    indices: Vec<u32>,
    material: Option<String>,
    /// The Unreal surface flags this part was built from, zero for meshes.
    poly_flags: u32,
}

#[derive(Clone, Default)]
struct MaterialGeometry {
    texture: Option<String>,
    two_sided: bool,
    blend: MaterialBlend,
    unlit: bool,
    emissive: bool,
    /// An Unreal shader takes its transparency from a second material rather
    /// than from the alpha of the texture it draws.
    opacity: Option<String>,
    /// The alpha an alpha-testing material cuts out below, from 0 to 1.
    alpha_ref: Option<f32>,
    /// The material whose alpha limits where a shader's self-illumination
    /// shines, such as the lit panes of a window in a stone wall.
    emissive_mask: Option<String>,
    /// A shader's specular layer, added over the surface whatever its opacity,
    /// and the material whose alpha limits where it shows.
    specular: Option<String>,
    specular_mask: Option<String>,
    /// The next frame of an animated texture, and how fast its frames run.
    next_frame: Option<String>,
    frame_rate: f32,
    /// The colour an Engine.ConstantColor stands for, alpha included.
    color: Option<[f32; 4]>,
    /// The colour an Engine.ColorModifier paints the material it wraps with.
    tint: Option<[f32; 4]>,
    /// How an Engine.TexPanner or Engine.TexOscillator moves the coordinates
    /// of the material it wraps.
    motion: UvMotion,
    /// An Engine.Combiner draws two materials at once, each moving its own way.
    combiner: Option<CombinerGeometry>,
    /// An Engine.VertexColor, which draws the mesh's own vertex colours.
    vertex_color: bool,
}

/// How a texture's coordinates travel over time: a steady scroll plus a wobble.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct UvMotion {
    pan: Vec2,
    oscillation_rate: Vec2,
    oscillation_amplitude: Vec2,
}

impl UvMotion {
    fn is_still(&self) -> bool {
        self.pan == Vec2::ZERO && self.oscillation_amplitude == Vec2::ZERO
    }

    fn then(self, other: Self) -> Self {
        Self {
            pan: self.pan + other.pan,
            oscillation_rate: if self.oscillation_amplitude == Vec2::ZERO {
                other.oscillation_rate
            } else {
                self.oscillation_rate
            },
            oscillation_amplitude: self.oscillation_amplitude + other.oscillation_amplitude,
        }
    }

    // Unreal's panner shifts the coordinates back along its direction, so
    // the image itself travels forward along it.
    fn offset(&self, seconds: f32) -> Vec2 {
        let phase = self.oscillation_rate * seconds * std::f32::consts::TAU;
        (-self.pan * seconds).fract()
            + self.oscillation_amplitude * Vec2::new(phase.x.sin(), phase.y.sin())
    }
}

#[derive(Clone, Debug)]
struct CombinerGeometry {
    second: String,
    operation: u32,
    modulate: f32,
    mask: Option<String>,
}

/// The second material an Engine.Combiner draws, resolved to its texture.
#[derive(Clone, Debug, PartialEq)]
struct SecondLayer {
    texture: String,
    motion: UvMotion,
    operation: u32,
    modulate: f32,
    /// Whether the two materials blend by the mesh's vertex alpha rather
    /// than by the second material's own.
    vertex_mask: bool,
}

#[derive(Clone, Copy, Debug, Default)]
enum MaterialBlend {
    #[default]
    Opaque,
    Mask,
    Blend,
    Add,
}

struct TextureGeometry {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

struct ResolvedTexture {
    texture: String,
    two_sided: bool,
    blend: MaterialBlend,
    unlit: bool,
    emissive: bool,
    opacity: Option<MaterialOpacity>,
    /// The texture whose alpha limits the self-illumination.
    emissive_mask: Option<String>,
    alpha_ref: Option<f32>,
    specular: Option<SpecularLayer>,
    motion: UvMotion,
    second: Option<SecondLayer>,
    tint: [f32; 4],
}

#[derive(Clone, Debug, PartialEq)]
enum MaterialOpacity {
    Constant(f32),
    Texture(String),
}

/// The textures an Unreal shader's specular layer is drawn from.
#[derive(Clone, Debug, PartialEq)]
struct SpecularLayer {
    texture: String,
    mask: Option<String>,
}

/// Materials that flip through an animated texture's frames, and how fast.
#[derive(Resource, Default)]
struct AnimatedMaterials(Vec<(Handle<StandardMaterial>, Vec<Handle<Image>>, f32)>);

/// Materials whose texture coordinates move, and how.
#[derive(Resource, Default)]
struct PanningMaterials(Vec<(Handle<StandardMaterial>, UvMotion)>);

struct MeshInstance {
    name: String,
    mesh: String,
    location: Vec3,
    rotation: Vec3,
    scale: Vec3,
    material_override: MaterialOverride,
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
struct MaterialOverride {
    alpha_blend: bool,
    two_sided: bool,
}

enum SceneLightKind {
    Point,
    Spot,
    Sun,
}

struct SceneLight {
    name: String,
    kind: SceneLightKind,
    location: Vec3,
    rotation: Vec3,
    color: Color,
    brightness: f32,
    radius: f32,
    cone: f32,
}

#[derive(Clone, Copy)]
struct ValueRange {
    min: f32,
    max: f32,
}

#[derive(Clone, Copy)]
struct VectorRange {
    min: Vec3,
    max: Vec3,
}

#[derive(Clone, Copy, Eq, Hash, PartialEq)]
enum ParticleBlend {
    Add,
    Alpha,
    Modulate,
}

enum ParticleShape {
    Box,
    Sphere,
    Polar,
}

enum VelocityDirection {
    Authored,
    Outward,
    Inward,
}

enum ParticleFacing {
    Camera,
    DirectionUp,
    DirectionNormal,
}

/// One Unreal particle emitter component, in viewer axes and Unreal units.
struct ParticleSystemDefinition {
    disabled: bool,
    texture: Option<String>,
    blend: ParticleBlend,
    max_particles: usize,
    lifetime: ValueRange,
    start_shape: ParticleShape,
    start_offset: Vec3,
    start_location: VectorRange,
    start_polar: VectorRange,
    sphere_radius: ValueRange,
    start_velocity: VectorRange,
    velocity_direction: VelocityDirection,
    acceleration: Vec3,
    velocity_loss: VectorRange,
    start_size: VectorRange,
    uniform_size: bool,
    size_scale: Vec<(f32, f32)>,
    color_scale: Vec<(f32, LinearRgba)>,
    color_scale_repeats: f32,
    color_multiplier: VectorRange,
    opacity: f32,
    fade_in_end: f32,
    fade_out_start: f32,
    facing: ParticleFacing,
    projection_normal: Vec3,
    spin: bool,
    start_spin: ValueRange,
    spins_per_second: ValueRange,
    counter_clockwise_chance: f32,
    subdivisions: UVec2,
    subdivision_start: u32,
    subdivision_end: u32,
    random_subdivision: bool,
    blend_subdivisions: bool,
}

/// An emitter component as placed by one Engine.Emitter actor.
struct ParticleEffect {
    name: String,
    location: Vec3,
    visible_limit: Option<ValueRange>,
    game_event: Option<i32>,
    definition: Arc<ParticleSystemDefinition>,
}

struct SceneDecal {
    name: String,
    material: String,
    location: Vec3,
    rotation: Vec3,
    scale: Vec3,
}

#[derive(Resource)]
struct LightingControl {
    base_brightness: f32,
    multiplier: f32,
    /// How much of the zone's daylight reaches the scene at this hour.
    daylight: f32,
}

impl LightingControl {
    fn brightness(&self) -> f32 {
        self.base_brightness * self.multiplier * self.daylight
    }
}

/// Lineage runs its sky on its own clock: the sun crosses the sky room while
/// the moon keeps to the far side, and the sunlight follows. The viewer's clock
/// runs at whatever speed the settings ask for.
#[derive(Resource)]
struct TimeOfDay {
    /// Hours since midnight.
    hour: f32,
    /// Real seconds a whole day lasts.
    day_length: f32,
}

impl Default for TimeOfDay {
    fn default() -> Self {
        Self {
            hour: 10.0,
            day_length: 300.0,
        }
    }
}

impl TimeOfDay {
    // Night keeps a little of the sun so the scene stays readable, the way
    // Lineage's own nights do.
    const NIGHT_DAYLIGHT: f32 = 0.18;
    const DAY_LENGTHS: [f32; 7] = [60.0, 120.0, 300.0, 600.0, 1_200.0, 1_800.0, 3_600.0];

    fn advance(&mut self, seconds: f32) {
        self.hour = (self.hour + seconds * 24.0 / self.day_length).rem_euclid(24.0);
    }

    fn skip_half_day(&mut self) {
        self.hour = (self.hour + 12.0).rem_euclid(24.0);
    }

    fn change_day_length(&mut self, longer: bool) {
        let lengths = Self::DAY_LENGTHS;
        let current = lengths
            .iter()
            .position(|length| *length >= self.day_length)
            .unwrap_or(lengths.len() - 1);
        let next = if longer {
            (current + 1).min(lengths.len() - 1)
        } else {
            current.saturating_sub(1)
        };
        self.day_length = lengths[next];
    }

    /// The sun's direction: on the eastern horizon at six, overhead along the
    /// sky room's own sun at noon, and below the world through the night.
    fn sun_direction(&self, path: &SunPath) -> Vec3 {
        let angle = (self.hour - 6.0) / 12.0 * std::f32::consts::PI;
        path.east * angle.cos() + path.noon * angle.sin()
    }

    /// How much daylight a sun this high lets through.
    fn daylight(sun_height: f32) -> f32 {
        let rise = ((sun_height + 0.1) / 0.4).clamp(0.0, 1.0);
        let rise = rise * rise * (3.0 - 2.0 * rise);
        Self::NIGHT_DAYLIGHT + (1.0 - Self::NIGHT_DAYLIGHT) * rise
    }
}

/// The great circle the sun travels, through the noon position the map gives
/// its sun and down to the horizon either side.
#[derive(Resource, Clone, Copy)]
struct SunPath {
    noon: Vec3,
    east: Vec3,
}

impl SunPath {
    fn through(noon: Vec3) -> Self {
        let noon = noon.normalize_or(Vec3::Y);
        let south = Vec3::new(noon.x, 0.0, noon.z).normalize_or(Vec3::Z);
        Self {
            noon,
            east: Vec3::Y.cross(south).normalize_or(Vec3::X),
        }
    }
}

impl Default for SunPath {
    fn default() -> Self {
        Self::through(Vec3::new(0.3, 0.8, 0.5))
    }
}

/// The walls the sun and moon travel inside, relative to the sky zone.
#[derive(Resource)]
struct SkyRoom {
    bounds: (Vec3, Vec3),
}

#[derive(Component)]
struct SkyBodyPhase {
    night: bool,
}

/// A directional light standing in for the sun, at its authored strength.
#[derive(Component)]
struct Daylight {
    illuminance: f32,
}

#[derive(Component)]
struct CoordinateReadout;

#[derive(Component)]
struct SelectionReadout;

#[derive(Component)]
struct TeleportPanel;

#[derive(Component)]
struct TeleportButton {
    location: Vec3,
}

#[derive(Component)]
struct TeleportScroll;

#[derive(Resource, Default)]
struct TeleportMenuState {
    open: bool,
}

#[derive(Resource)]
struct MapNavigation {
    maps_dir: PathBuf,
    radar: Option<RadarAtlas>,
    next: Arc<Mutex<Option<PathBuf>>>,
}

struct RadarAtlas {
    rgba: Vec<u8>,
}

#[derive(Resource, Default)]
struct WorldMapState {
    open: bool,
}

#[derive(Resource)]
struct SettingsMenuState {
    open: bool,
    fog: Option<SceneFog>,
    fog_enabled: bool,
    initial_move_speed: f32,
}

/// Emitters the map binds to a server-fired game event. They are dormant until
/// the viewer is asked to play them, one event at a time or all at once.
#[derive(Resource, Default)]
struct GameEventEffects {
    counts: BTreeMap<i32, usize>,
    playing: BTreeSet<i32>,
}

impl GameEventEffects {
    fn plays(&self, event: Option<i32>) -> bool {
        event.is_none_or(|event| self.playing.contains(&event))
    }

    fn toggle(&mut self, event: i32) {
        if !self.playing.remove(&event) {
            self.playing.insert(event);
        }
    }

    fn toggle_all(&mut self) {
        if self.playing.is_empty() {
            self.playing = self.counts.keys().copied().collect();
        } else {
            self.playing.clear();
        }
    }

    fn playing_components(&self) -> usize {
        self.counts
            .iter()
            .filter(|(event, _)| self.playing.contains(event))
            .map(|(_, count)| count)
            .sum()
    }

    fn components(&self) -> usize {
        self.counts.values().sum()
    }
}

#[derive(Component)]
struct GameEventPanel;

#[derive(Component)]
struct GameEventScroll;

#[derive(Component, Clone, Copy)]
struct GameEventRow(i32);

#[derive(Component, Clone, Copy)]
struct GameEventRowText(i32);

#[derive(Resource, Default)]
struct GameEventMenuState {
    open: bool,
}

#[derive(Component)]
struct SettingsPanel;

#[derive(Component, Clone, Copy, PartialEq, Eq)]
enum OverlayPanelKind {
    Settings,
    Teleport,
    WorldMap,
    GameEvents,
}

#[derive(Component, Clone, Copy)]
enum SettingsAction {
    ToggleFog,
    ToggleDayNight,
    ShorterDay,
    LongerDay,
    ToggleGameEventEffects,
    Slower,
    Faster,
    ResetSpeed,
}

/// The settings rows whose text follows the state they control.
#[derive(Component, Clone, Copy)]
enum SettingsLabel {
    Fog,
    DayNight,
    GameEvents,
    Speed,
}

#[derive(Component)]
struct WorldMapPanel;

#[derive(Component)]
struct WorldMapImage;

#[derive(Component)]
struct WorldMapHoverText;

#[derive(Component)]
struct SelectableObject {
    actor: String,
    object: String,
    material: Option<String>,
    unreal_location: Vec3,
}

#[derive(Resource)]
struct SelectionState {
    selected: Option<(Entity, Handle<StandardMaterial>)>,
    highlight: Handle<StandardMaterial>,
    cursor: Option<Vec2>,
    hits: Vec<Entity>,
    hit_index: usize,
}

#[derive(SystemParam)]
struct SelectionView<'w, 's> {
    window: Single<'w, 's, &'static Window>,
    camera: Single<'w, 's, (&'static Camera, &'static GlobalTransform), With<FlyCamera>>,
    buttons: Res<'w, ButtonInput<MouseButton>>,
    keys: Res<'w, ButtonInput<KeyCode>>,
    teleport_menu: Res<'w, TeleportMenuState>,
    world_map: Res<'w, WorldMapState>,
    settings_menu: Res<'w, SettingsMenuState>,
    game_event_menu: Res<'w, GameEventMenuState>,
}

#[derive(SystemParam)]
struct SettingsControls<'w> {
    menu: ResMut<'w, SettingsMenuState>,
    events: ResMut<'w, GameEventEffects>,
    time: ResMut<'w, TimeOfDay>,
}

#[derive(SystemParam)]
struct FlyCameraInput<'w> {
    time: Res<'w, Time>,
    keys: Res<'w, ButtonInput<KeyCode>>,
    mouse_buttons: Res<'w, ButtonInput<MouseButton>>,
    mouse_motion: Res<'w, AccumulatedMouseMotion>,
    teleport_menu: Res<'w, TeleportMenuState>,
    world_map: Res<'w, WorldMapState>,
    settings_menu: Res<'w, SettingsMenuState>,
    game_event_menu: Res<'w, GameEventMenuState>,
}

#[derive(Component)]
struct AnimatedSkeleton {
    animation: Arc<AnimationGeometry>,
    joints: Vec<Entity>,
}

fn lighting_multiplier(current: f32, direction: i8) -> f32 {
    let factor = 1.25_f32.powi(i32::from(direction));
    (current * factor).clamp(0.05, 64.0)
}

fn has_alpha_channel(texture: &TextureGeometry) -> bool {
    texture.rgba.chunks_exact(4).any(|pixel| pixel[3] < u8::MAX)
}

fn sample_rotation(track: &AnimationTrackGeometry, time: f32) -> Quat {
    let (left, right, fraction) = sample_segment(&track.times, track.rotations.len(), time);
    track.rotations[left].slerp(track.rotations[right], fraction)
}

fn sample_translation(track: &AnimationTrackGeometry, time: f32) -> Vec3 {
    let (left, right, fraction) = sample_segment(&track.times, track.translations.len(), time);
    track.translations[left].lerp(track.translations[right], fraction)
}

fn sample_segment(times: &[f32], value_count: usize, time: f32) -> (usize, usize, f32) {
    if value_count <= 1 || times.len() <= 1 {
        return (0, 0, 0.0);
    }
    let right = times
        .iter()
        .take(value_count)
        .position(|sample| *sample >= time)
        .unwrap_or(value_count - 1);
    let left = right.saturating_sub(1);
    let span = (times[right] - times[left]).max(f32::EPSILON);
    (left, right, ((time - times[left]) / span).clamp(0.0, 1.0))
}

fn unreal_rotation(rotation: Vec3) -> Quat {
    Quat::from_rotation_y(rotation.y)
        * Quat::from_rotation_z(rotation.x)
        * Quat::from_rotation_x(rotation.z)
}

fn smooth_normals(positions: &[[f32; 3]], indices: &[u32]) -> Vec<[f32; 3]> {
    let mut normals = vec![Vec3::ZERO; positions.len()];
    for triangle in indices.chunks_exact(3) {
        let [a, b, c] = [
            triangle[0] as usize,
            triangle[1] as usize,
            triangle[2] as usize,
        ];
        let normal = (Vec3::from_array(positions[b]) - Vec3::from_array(positions[a]))
            .cross(Vec3::from_array(positions[c]) - Vec3::from_array(positions[a]));
        normals[a] += normal;
        normals[b] += normal;
        normals[c] += normal;
    }
    normals
        .into_iter()
        .map(|normal| normal.normalize_or_zero().to_array())
        .collect()
}

// Terrain vertex colours are baked lighting, and a map that never baked it
// leaves them black but for a few stray vertices. Multiplying by those would
// paint the whole terrain black.
fn terrain_vertex_colors_are_meaningful(colors: &[[f32; 4]]) -> bool {
    let lit = colors
        .iter()
        .filter(|color| color[0] + color[1] + color[2] > 0.1)
        .count();
    lit * 2 > colors.len()
}

fn terrain_indices(
    width: usize,
    height: usize,
    quad_visibility: &[u8],
    edge_turns: &[u8],
) -> Vec<u32> {
    let mut indices = Vec::with_capacity((width - 1) * (height - 1) * 6);
    for y in 0..height - 1 {
        for x in 0..width - 1 {
            let bit = y * width + x;
            if !bitmap_bit(quad_visibility, bit, true) {
                continue;
            }
            let top_left = bit as u32;
            let top_right = top_left + 1;
            let bottom_left = top_left + width as u32;
            let bottom_right = bottom_left + 1;
            if bitmap_bit(edge_turns, bit, false) {
                indices.extend_from_slice(&[
                    top_left,
                    top_right,
                    bottom_left,
                    top_right,
                    bottom_right,
                    bottom_left,
                ]);
            } else {
                indices.extend_from_slice(&[
                    top_left,
                    bottom_right,
                    bottom_left,
                    top_left,
                    top_right,
                    bottom_right,
                ]);
            }
        }
    }
    indices
}

fn bitmap_bit(bitmap: &[u8], bit: usize, default: bool) -> bool {
    bitmap
        .get(bit / 8)
        .map_or(default, |byte| byte & (1 << (bit % 8)) != 0)
}

fn distance_fog(fog: &SceneFog) -> DistanceFog {
    DistanceFog {
        color: fog.color,
        directional_light_color: Color::NONE,
        directional_light_exponent: 0.0,
        falloff: FogFalloff::Linear {
            start: fog.start,
            end: fog.end,
        },
    }
}

fn adjusted_move_speed(current: f32, direction: i8) -> f32 {
    (current * 2.0_f32.powi(i32::from(direction))).clamp(1.0, 2_000.0)
}

#[derive(Component)]
struct FlyCamera {
    yaw: f32,
    pitch: f32,
    speed: f32,
}

fn selection_hit_index(
    current: usize,
    hit_count: usize,
    same_stack: bool,
    backward: bool,
) -> usize {
    if !same_stack {
        return if backward { hit_count - 1 } else { 0 };
    }
    if backward {
        (current + hit_count - 1) % hit_count
    } else {
        (current + 1) % hit_count
    }
}

fn selection_description(selected: &SelectableObject) -> String {
    format!(
        "Actor: {}\nObject: {}\nMaterial: {}\nLocation: X {:.0}  Y {:.0}  Z {:.0}",
        selected.actor,
        selected.object,
        selected.material.as_deref().unwrap_or("<none>"),
        selected.unreal_location.x,
        selected.unreal_location.y,
        selected.unreal_location.z,
    )
}

fn camera_unreal_coordinates(viewer_position: Vec3, scene_origin: Vec3) -> Vec3 {
    let absolute = viewer_position * UNREAL_UNITS_PER_METER + scene_origin;
    Vec3::new(absolute.x, -absolute.z, absolute.y)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::*;

    #[test]
    fn movement_speed_controls_cover_slow_and_fast_inspection() {
        assert_eq!(adjusted_move_speed(30.0, -1), 15.0);
        assert_eq!(adjusted_move_speed(1.0, -1), 1.0);
        assert_eq!(adjusted_move_speed(1_500.0, 1), 2_000.0);
    }

    #[test]
    fn radar_pages_and_pixels_map_to_lineage_regions() {
        assert_eq!(radar_page_number("RadarMap_1"), Some(1));
        assert_eq!(radar_page_number("radarmap_20"), Some(20));
        assert_eq!(radar_page_number("WorldMap_1"), None);
        assert_eq!(radar_map_name(Vec2::splat(-0.5)), "10_10.unr");
        assert_eq!(
            radar_map_name(Vec2::new(
                -0.5 + 246.0 / (RADAR_PAGE_SIZE * RADAR_COLUMNS) as f32,
                -0.5 + 246.0 / (RADAR_PAGE_SIZE * RADAR_ROWS) as f32,
            )),
            "11_11.unr"
        );
        assert_eq!(radar_map_name(Vec2::splat(0.499_999)), "30_26.unr");

        let tile_center = Vec2::new(
            (14.0 * RADAR_TILE_SIZE + RADAR_TILE_SIZE / 2.0)
                / (RADAR_PAGE_SIZE * RADAR_COLUMNS) as f32
                - 0.5,
            (10.0 * RADAR_TILE_SIZE + RADAR_TILE_SIZE / 2.0)
                / (RADAR_PAGE_SIZE * RADAR_ROWS) as f32
                - 0.5,
        );
        assert_eq!(radar_map_name(tile_center), "24_20.unr");
    }

    #[test]
    fn selected_map_survives_navigation_resource_teardown() {
        let handoff = Arc::new(Mutex::new(None));
        {
            let navigation = MapNavigation {
                maps_dir: PathBuf::from("Maps"),
                radar: None,
                next: handoff.clone(),
            };
            store_next_map(&navigation.next, PathBuf::from("Maps/15_10.unr")).unwrap();
        }
        assert_eq!(
            take_next_map(&handoff).unwrap(),
            Some(PathBuf::from("Maps/15_10.unr"))
        );
    }

    #[test]
    #[ignore = "requires LINEAGE2_CLIENT to point to a complete client"]
    fn loads_lineage_radar_atlas() {
        let client = PathBuf::from(
            std::env::var_os("LINEAGE2_CLIENT").expect("LINEAGE2_CLIENT is not configured"),
        );
        let atlas = load_radar_atlas(&client.join("SysTextures/L2_RadarMap.utx")).unwrap();
        assert_eq!(
            atlas.rgba.len(),
            RADAR_PAGE_SIZE * RADAR_COLUMNS * RADAR_PAGE_SIZE * RADAR_ROWS * 4
        );
    }

    #[test]
    fn terrain_grid_produces_expected_triangle_count() {
        let positions = vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 0.0, -1.0],
            [1.0, 0.0, -1.0],
        ];
        let indices = [0, 3, 2, 0, 1, 3];
        let normals = smooth_normals(&positions, &indices);
        assert_eq!(normals.len(), 4);
        assert!(normals.iter().all(|normal| normal[1] > 0.99));
    }

    #[test]
    fn unset_black_terrain_vertex_colors_do_not_mask_the_texture() {
        assert!(!terrain_vertex_colors_are_meaningful(
            &[[0.0, 0.0, 0.0, 1.0]; 4]
        ));
        assert!(terrain_vertex_colors_are_meaningful(&[
            [0.0, 0.0, 0.0, 1.0],
            [0.25, 0.25, 0.25, 1.0],
            [0.5, 0.5, 0.5, 1.0],
        ]));
        // A few stray lit vertices on an unlit terrain are not lighting.
        let mut stray = vec![[0.0, 0.0, 0.0, 1.0]; 100];
        stray[7] = [0.4, 0.3, 0.2, 1.0];
        assert!(!terrain_vertex_colors_are_meaningful(&stray));
    }

    #[test]
    fn terrain_visibility_bitmap_cuts_hidden_quads() {
        let indices = terrain_indices(3, 2, &[0b0000_0101], &[]);
        assert_eq!(indices, [0, 4, 3, 0, 1, 4]);
    }

    #[test]
    fn terrain_edge_turn_bitmap_selects_the_other_diagonal() {
        let indices = terrain_indices(2, 2, &[1], &[1]);
        assert_eq!(indices, [0, 1, 2, 1, 3, 2]);
    }

    #[test]
    fn g16_heightmap_is_reconstructed_with_terrain_coordinates() {
        let coordinates = Coordinates {
            origin: Vector3 {
                x: -1.0,
                y: -2.0,
                z: 10.0,
            },
            x_axis: Vector3 {
                x: 2.0,
                y: 0.0,
                z: 0.0,
            },
            y_axis: Vector3 {
                x: 0.0,
                y: 3.0,
                z: 0.0,
            },
            z_axis: Vector3 {
                x: 0.0,
                y: 0.0,
                z: 0.5,
            },
        };
        let bytes = [10_u16, 12, 14, 16]
            .into_iter()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>();
        let terrain = terrain_from_g16_heightmap(
            2,
            2,
            &bytes,
            &coordinates,
            Vec::new(),
            TerrainLayering::default(),
            TerrainTopology::default(),
        )
        .unwrap();

        assert_eq!(terrain.positions.len(), 4);
        assert_eq!(terrain.positions[0], [2.0, 0.0, -6.0]);
        assert_eq!(terrain.positions[3], [4.0, 3.0, -9.0]);
    }

    #[test]
    fn unreal_yaw_rotates_x_toward_mapped_y() {
        let rotation = unreal_rotation(Vec3::new(0.0, std::f32::consts::FRAC_PI_2, 0.0));
        assert!(rotation.mul_vec3(Vec3::X).distance(Vec3::NEG_Z) < 0.000_001);
    }

    #[test]
    fn a_terrain_paints_every_layer_through_its_alpha_map() {
        let properties = vec![
            PropertyDump {
                name: "TerrainScale".into(),
                property_type: "Vector".into(),
                array_index: 0,
                size: 12,
                value: "128,128,76".into(),
                raw: String::new(),
                wire_header: String::new(),
            },
            PropertyDump {
                name: "Layers".into(),
                property_type: "TerrainLayer".into(),
                array_index: 0,
                size: 0,
                value: r#"{"Scale":"128,128,76","Texture":"T_ADEN.AG_001","AlphaMap":"T_texture.Texture.layer0","UScale":"1","VScale":"1"}"#.into(),
                raw: String::new(),
                wire_header: String::new(),
            },
            PropertyDump {
                name: "Layers".into(),
                property_type: "TerrainLayer".into(),
                array_index: 1,
                size: 0,
                value: r#"{"AlphaMap":"T_23_22.Height.23_22_S1","Texture":"T_Innadrile.INNS_06","UScale":"2","VScale":"0.5"}"#.into(),
                raw: String::new(),
                wire_header: String::new(),
            },
            PropertyDump {
                name: "Layers".into(),
                property_type: "TerrainLayer".into(),
                array_index: 2,
                size: 0,
                value: r#"{"AlphaMap":"","Texture":""}"#.into(),
                raw: String::new(),
                wire_header: String::new(),
            },
        ];
        let layering = terrain_layers(&properties);
        assert_eq!(layering.quad_size, Vec2::splat(128.0));
        assert_eq!(
            layering.layers,
            vec![
                TerrainLayer {
                    texture: "T_ADEN.AG_001".into(),
                    alpha_map: Some("T_texture.Texture.layer0".into()),
                    scale: Vec2::ONE,
                },
                TerrainLayer {
                    texture: "T_Innadrile.INNS_06".into(),
                    alpha_map: Some("T_23_22.Height.23_22_S1".into()),
                    scale: Vec2::new(2.0, 0.5),
                },
            ]
        );
        // A 256-texel texture at UScale 1 spans two 128-unit quads.
        let texture = TextureGeometry {
            width: 256,
            height: 256,
            rgba: Vec::new(),
        };
        assert_eq!(
            terrain::layer_repeats(layering.quad_size, &layering.layers[0], &texture),
            Vec2::splat(0.5)
        );
    }

    #[test]
    fn an_eight_bit_texture_takes_its_colours_from_its_palette() {
        // A compact count of 256, then BGRA colours.
        let mut data = vec![0x40, 0x04];
        for index in 0..=255_u8 {
            data.extend([index, index / 2, 7, 255 - index]);
        }
        let palette = scene::decode_palette(&data).unwrap();
        assert_eq!(palette.len(), 256);
        assert_eq!(palette[10], [7, 5, 10, 245]);
        assert_eq!(scene::decode_palette(&[0x40]), None);
    }

    #[test]
    fn a_reference_missing_from_its_package_is_read_as_another_package() {
        let package_files = BTreeMap::from([
            ("schtgart_fielddeco_t".to_owned(), ()),
            ("rachiq_habitat_t".to_owned(), ()),
        ]);
        assert_eq!(
            other_package_reading(
                "rachiq_habitat_t.schtgart_fielddeco_t.schtgart_tomb_cliff_cs",
                &package_files
            ),
            Some("schtgart_fielddeco_t.schtgart_tomb_cliff_cs".to_owned())
        );
        // A group that is not a package name stays a group.
        assert_eq!(
            other_package_reading("rachiq_habitat_t.field.rock", &package_files),
            None
        );
        assert_eq!(
            other_package_reading("rachiq_habitat_t.rock", &package_files),
            None
        );
    }

    #[test]
    fn a_reference_out_of_its_package_keeps_the_package_it_names() {
        // A group inside the package being read is qualified with it.
        assert_eq!(
            qualify_object_path("fx_e_t", "fallwater.aden_fall01"),
            "fx_e_t.fallwater.aden_fall01"
        );
        assert_eq!(
            qualify_object_path("new_speaking_v_t", "Newbie_fallwater01_pan"),
            "new_speaking_v_t.newbie_fallwater01_pan"
        );
        // A path that already names a package is left alone.
        assert_eq!(
            qualify_object_path("new_speaking_v_t", "FX_E_T.fallwater.anghel_fall_w011"),
            "fx_e_t.fallwater.anghel_fall_w011"
        );
        assert_eq!(
            qualify_object_path("fx_e_t", "FX_E_T.fallwater.anghel_fall_w011"),
            "fx_e_t.fallwater.anghel_fall_w011"
        );
    }

    #[test]
    fn external_mesh_paths_map_to_package_qualified_aliases() {
        assert_eq!(
            external_package_name("demev_S.tree.curse_basictree").as_deref(),
            Some("demev_s")
        );
        assert_eq!(
            external_mesh_aliases("demev_s", "tree.curse_basictree"),
            ["tree.curse_basictree", "demev_s.tree.curse_basictree"]
        );
    }

    #[test]
    fn embedded_mesh_materials_are_qualified_by_their_package() {
        assert_eq!(
            qualify_material_paths(
                "sp_lighthouse",
                vec![
                    "sp_lighthouse009".into(),
                    String::new(),
                    "other_package.material".into(),
                ],
            ),
            [
                "sp_lighthouse.sp_lighthouse009",
                "",
                "other_package.material",
            ]
        );
    }

    #[test]
    fn actor_extent_can_center_a_map_without_terrain_vertices() {
        let instances = [
            MeshInstance {
                name: "first".into(),
                mesh: "package.first".into(),
                location: Vec3::new(100.0, 20.0, -40.0),
                rotation: Vec3::ZERO,
                scale: Vec3::ONE,
                material_override: MaterialOverride::default(),
            },
            MeshInstance {
                name: "second".into(),
                mesh: "package.second".into(),
                location: Vec3::new(300.0, 60.0, -80.0),
                rotation: Vec3::ZERO,
                scale: Vec3::ONE,
                material_override: MaterialOverride::default(),
            },
        ];
        let (center, radius) = scene_extent(&None, &instances);
        assert_eq!(center, Vec3::new(200.0, 40.0, -60.0));
        assert_eq!(radius, 100.0);
    }

    #[test]
    fn bgra8_textures_are_converted_to_rgba8() {
        assert_eq!(
            decode_texture(5, 1, 1, &[10, 20, 30, 40]),
            Some(vec![30, 20, 10, 40])
        );
    }

    #[test]
    fn lineage_mip_zero_color_becomes_a_single_pixel_texture() {
        let property = PropertyDump {
            name: "MipZero".into(),
            property_type: "Color".into(),
            array_index: 0,
            size: 4,
            value: String::new(),
            raw: "1518193f".into(),
            wire_header: String::new(),
        };
        let texture = mip_zero_texture(&[property], 5).unwrap();
        assert_eq!((texture.width, texture.height), (1, 1));
        assert_eq!(texture.rgba, [0x19, 0x18, 0x15, 0x3f]);
    }

    #[test]
    fn dxt1_texture_blocks_are_expanded() {
        let rgba = decode_texture(3, 4, 4, &[0; 8]).unwrap();
        assert_eq!(rgba.len(), 4 * 4 * 4);
        assert!(rgba.iter().all(|value| *value == 0 || *value == 255));
    }

    #[test]
    fn unreal_material_textures_repeat_outside_unit_uvs() {
        let image = repeating_texture_image(&TextureGeometry {
            width: 1,
            height: 1,
            rgba: vec![255; 4],
        });
        let ImageSampler::Descriptor(sampler) = image.sampler else {
            panic!("texture did not receive a sampler descriptor");
        };
        assert_eq!(sampler.address_mode_u, ImageAddressMode::Repeat);
        assert_eq!(sampler.address_mode_v, ImageAddressMode::Repeat);
    }

    #[test]
    fn lineage_shader_output_blending_maps_to_transparency() {
        let property = |value: &str| PropertyDump {
            name: "OutputBlending".into(),
            property_type: "Byte".into(),
            array_index: 0,
            size: 1,
            value: value.into(),
            raw: String::new(),
            wire_header: String::new(),
        };

        assert!(matches!(
            material_blend(&[property("3")]),
            MaterialBlend::Blend
        ));
        assert!(matches!(
            material_blend(&[property("5")]),
            MaterialBlend::Add
        ));
    }

    #[test]
    fn masked_texture_property_enables_alpha_cutouts() {
        let property = PropertyDump {
            name: "bMasked".into(),
            property_type: "Bool".into(),
            array_index: 0,
            size: 0,
            value: "true".into(),
            raw: String::new(),
            wire_header: String::new(),
        };
        assert!(matches!(material_blend(&[property]), MaterialBlend::Mask));
    }

    #[test]
    fn lineage_texture_two_sided_property_disables_culling() {
        let property = PropertyDump {
            name: "bTwoSided".into(),
            property_type: "Bool".into(),
            array_index: 0,
            size: 0,
            value: "true".into(),
            raw: String::new(),
            wire_header: String::new(),
        };
        assert!(material_is_two_sided(&[property]));
    }

    #[test]
    fn direct_texture_preserves_its_material_flags() {
        let materials = HashMap::from([(
            "package.twosided".into(),
            MaterialGeometry {
                texture: Some("package.twosided".into()),
                two_sided: true,
                blend: MaterialBlend::Mask,
                ..default()
            },
        )]);
        let resolved = resolve_texture_from(
            &materials,
            |name| name == "package.twosided",
            "package.twosided",
        )
        .unwrap();
        assert!(resolved.two_sided);
        assert!(matches!(resolved.blend, MaterialBlend::Mask));
    }

    #[test]
    fn a_shader_takes_its_transparency_and_scrolling_from_its_own_materials() {
        let materials = HashMap::from([
            (
                "water.waterfall".into(),
                MaterialGeometry {
                    texture: Some("water.fall_pan".into()),
                    opacity: Some("water.water_color".into()),
                    ..default()
                },
            ),
            (
                "water.fall_pan".into(),
                MaterialGeometry {
                    texture: Some("water.fall".into()),
                    motion: UvMotion {
                        pan: Vec2::new(1.1, 0.0),
                        ..default()
                    },
                    ..default()
                },
            ),
            (
                "water.water_color".into(),
                MaterialGeometry {
                    color: Some([0.05, 0.47, 0.89, 0.094]),
                    ..default()
                },
            ),
        ]);
        let resolved =
            resolve_texture_from(&materials, |name| name == "water.fall", "water.waterfall")
                .unwrap();

        assert_eq!(resolved.texture, "water.fall");
        assert_eq!(resolved.motion.pan, Vec2::new(1.1, 0.0));
        assert_eq!(resolved.opacity, Some(MaterialOpacity::Constant(0.094)));
    }

    #[test]
    fn an_alpha_tested_shader_stays_a_cutout_at_its_own_threshold() {
        let materials = HashMap::from([(
            "field.flower_sh".into(),
            MaterialGeometry {
                texture: Some("field.flower".into()),
                opacity: Some("field.flower".into()),
                blend: MaterialBlend::Mask,
                alpha_ref: Some(15.0 / 255.0),
                ..default()
            },
        )]);
        let resolved =
            resolve_texture_from(&materials, |name| name == "field.flower", "field.flower_sh")
                .unwrap();
        let material = standard_material(
            &resolved,
            Handle::default(),
            None,
            false,
            MaterialOverride::default(),
        );

        assert!(resolved.opacity.is_some());
        assert_eq!(material.alpha_mode, AlphaMode::Mask(15.0 / 255.0));
    }

    #[test]
    fn a_constant_opacity_replaces_the_alpha_of_the_texture_it_draws() {
        // A waterfall sheet whose diffuse is mostly transparent: the shader's
        // constant opacity alone decides how solid it is.
        let sheet = TextureGeometry {
            width: 2,
            height: 1,
            rgba: vec![60, 120, 200, 40, 90, 150, 230, 0],
        };
        let image = crate::scene::opaque_texture_image(&sheet);
        assert_eq!(
            image.data.as_deref(),
            Some([60, 120, 200, 255, 90, 150, 230, 255].as_slice())
        );
    }

    #[test]
    fn a_self_illumination_mask_limits_the_glow_to_what_it_marks() {
        let materials = HashMap::from([(
            "village.wall_light".into(),
            MaterialGeometry {
                texture: Some("village.wall".into()),
                emissive: true,
                emissive_mask: Some("village.wall".into()),
                ..default()
            },
        )]);
        let resolved = resolve_texture_from(
            &materials,
            |name| name == "village.wall",
            "village.wall_light",
        )
        .unwrap();
        assert_eq!(resolved.emissive_mask.as_deref(), Some("village.wall"));

        // Stone around a window pane: only the pane's alpha lets light out.
        let wall = TextureGeometry {
            width: 2,
            height: 1,
            rgba: vec![200, 180, 160, 0, 240, 220, 120, 255],
        };
        let glow = crate::scene::self_illumination_image(&wall, &wall);
        assert_eq!(
            glow.data.as_deref(),
            Some([0, 0, 0, 255, 240, 220, 120, 255].as_slice())
        );
    }

    #[test]
    fn a_shader_draws_its_specular_layer_over_whatever_opacity_it_has() {
        let materials = HashMap::from([
            (
                "village.waterfall".into(),
                MaterialGeometry {
                    texture: Some("fx.fall".into()),
                    opacity: Some("village.water_color".into()),
                    specular: Some("fx.flow_0001".into()),
                    specular_mask: Some("fx.flow_0001".into()),
                    ..default()
                },
            ),
            (
                "village.water_color".into(),
                MaterialGeometry {
                    color: Some([0.05, 0.47, 0.89, 0.094]),
                    ..default()
                },
            ),
        ]);
        let resolved = resolve_texture_from(
            &materials,
            |name| matches!(name, "fx.fall" | "fx.flow_0001"),
            "village.waterfall",
        )
        .unwrap();

        assert_eq!(resolved.opacity, Some(MaterialOpacity::Constant(0.094)));
        assert_eq!(
            resolved.specular,
            Some(SpecularLayer {
                texture: "fx.flow_0001".into(),
                mask: Some("fx.flow_0001".into()),
            })
        );
    }

    #[test]
    fn an_animated_texture_plays_its_frames_until_the_chain_loops() {
        let frame = |next: &str| MaterialGeometry {
            texture: Some(String::new()),
            next_frame: Some(next.into()),
            frame_rate: 15.0,
            ..default()
        };
        let materials = HashMap::from([
            ("fx.flow_0001".into(), frame("fx.flow_0002")),
            ("fx.flow_0002".into(), frame("fx.flow_0003")),
            ("fx.flow_0003".into(), frame("fx.flow_0001")),
        ]);

        let (frames, rate) = crate::scene::frame_chain(&materials, |_| true, "fx.flow_0001");
        assert_eq!(frames, ["fx.flow_0001", "fx.flow_0002", "fx.flow_0003"]);
        assert_eq!(rate, 15.0);

        // A frame the client does not ship ends the animation early.
        let (frames, _) =
            crate::scene::frame_chain(&materials, |name| name != "fx.flow_0003", "fx.flow_0001");
        assert_eq!(frames, ["fx.flow_0001", "fx.flow_0002"]);
    }

    #[test]
    fn a_combiner_resolves_both_of_its_moving_layers() {
        // Lineage's fountain pool: a shader over a combiner of two panners,
        // the second of them also wobbling.
        let materials = HashMap::from([
            (
                "orbis.gallery_water".into(),
                MaterialGeometry {
                    texture: Some("orbis.water_com".into()),
                    ..default()
                },
            ),
            (
                "orbis.water_com".into(),
                MaterialGeometry {
                    texture: Some("orbis.water_pan2".into()),
                    combiner: Some(CombinerGeometry {
                        second: "orbis.water_pan_osc".into(),
                        operation: 3,
                        modulate: 2.0,
                        mask: None,
                    }),
                    ..default()
                },
            ),
            (
                "orbis.water_pan2".into(),
                MaterialGeometry {
                    texture: Some("fx.water02".into()),
                    motion: UvMotion {
                        pan: Vec2::new(0.015, 0.0),
                        ..default()
                    },
                    ..default()
                },
            ),
            (
                "orbis.water_pan_osc".into(),
                MaterialGeometry {
                    texture: Some("orbis.water_pan".into()),
                    motion: UvMotion {
                        oscillation_rate: Vec2::new(0.5, 0.35),
                        oscillation_amplitude: Vec2::new(0.001, 0.01),
                        ..default()
                    },
                    ..default()
                },
            ),
            (
                "orbis.water_pan".into(),
                MaterialGeometry {
                    texture: Some("fx.ocean030".into()),
                    motion: UvMotion {
                        pan: Vec2::new(0.03, 0.0),
                        ..default()
                    },
                    ..default()
                },
            ),
        ]);
        let resolved = resolve_texture_from(
            &materials,
            |name| matches!(name, "fx.water02" | "fx.ocean030"),
            "orbis.gallery_water",
        )
        .unwrap();

        assert_eq!(resolved.texture, "fx.water02");
        assert_eq!(resolved.motion.pan, Vec2::new(0.015, 0.0));
        let second = resolved.second.unwrap();
        assert_eq!(second.texture, "fx.ocean030");
        assert_eq!((second.operation, second.modulate), (3, 2.0));
        assert_eq!(second.motion.pan, Vec2::new(0.03, 0.0));
        assert_eq!(second.motion.oscillation_amplitude, Vec2::new(0.001, 0.01));
    }

    #[test]
    fn a_combiner_masked_by_vertex_colour_blends_by_the_painted_alpha() {
        // The mouth of the Rachiq habitat cave: cliff rock blended into the
        // cave wall wherever the mesh was painted.
        let materials = HashMap::from([
            (
                "rachiq_habitat_t.rachiq_01_sh".into(),
                MaterialGeometry {
                    texture: Some("schtgart_fielddeco_t.schtgart_tomb_cliff_cs".into()),
                    combiner: Some(CombinerGeometry {
                        second: "rachiq_habitat_t.rachiq_01".into(),
                        operation: 5,
                        modulate: 1.0,
                        mask: Some("schtgart_fielddeco_t.schtgart_tomb_cliff_light_vc".into()),
                    }),
                    ..default()
                },
            ),
            (
                "schtgart_fielddeco_t.schtgart_tomb_cliff_cs".into(),
                MaterialGeometry {
                    texture: Some("schtgart_fielddeco_t.schtgart_tomb_cliff".into()),
                    ..default()
                },
            ),
            (
                "schtgart_fielddeco_t.schtgart_tomb_cliff_light_vc".into(),
                MaterialGeometry {
                    vertex_color: true,
                    ..default()
                },
            ),
        ]);
        let resolved = resolve_texture_from(
            &materials,
            |name| {
                matches!(
                    name,
                    "schtgart_fielddeco_t.schtgart_tomb_cliff" | "rachiq_habitat_t.rachiq_01"
                )
            },
            "rachiq_habitat_t.rachiq_01_sh",
        )
        .unwrap();

        assert_eq!(resolved.texture, "schtgart_fielddeco_t.schtgart_tomb_cliff");
        let second = resolved.second.unwrap();
        assert_eq!(second.texture, "rachiq_habitat_t.rachiq_01");
        assert!(second.vertex_mask);
    }

    #[test]
    fn uv_motion_scrolls_and_wobbles_from_the_clock() {
        let motion = UvMotion {
            pan: Vec2::new(0.25, 0.0),
            oscillation_rate: Vec2::new(0.0, 1.0),
            oscillation_amplitude: Vec2::new(0.0, 0.1),
        };

        assert_eq!(motion.offset(0.0), Vec2::ZERO);
        // The coordinates move back so the image moves forward along the pan,
        // and a quarter of the way through its cycle the wobble peaks.
        let offset = motion.offset(0.25);
        assert!((offset.x + 0.0625).abs() < 1.0e-6);
        assert!((offset.y - 0.1).abs() < 1.0e-6);
        // A long-running scroll stays within one tile.
        assert!(motion.offset(1_000.0).x.abs() < 1.0);
        assert!(UvMotion::default().is_still());
    }

    #[test]
    fn a_shader_masked_by_a_texture_names_the_texture_to_take_alpha_from() {
        let materials = HashMap::from([
            (
                "water.surface".into(),
                MaterialGeometry {
                    texture: Some("water.diffuse".into()),
                    opacity: Some("water.mask_pan".into()),
                    ..default()
                },
            ),
            (
                "water.mask_pan".into(),
                MaterialGeometry {
                    texture: Some("water.mask".into()),
                    ..default()
                },
            ),
        ]);
        let resolved = resolve_texture_from(
            &materials,
            |name| matches!(name, "water.diffuse" | "water.mask"),
            "water.surface",
        )
        .unwrap();

        assert_eq!(resolved.texture, "water.diffuse");
        assert_eq!(
            resolved.opacity,
            Some(MaterialOpacity::Texture("water.mask".into()))
        );
    }

    #[test]
    fn night_dims_the_zone_without_losing_its_lighting_controls() {
        let mut control = LightingControl {
            base_brightness: 300.0,
            multiplier: 2.0,
            daylight: TimeOfDay::daylight(1.0),
        };
        assert_eq!(control.brightness(), 600.0);

        control.daylight = TimeOfDay::daylight(-1.0);
        assert!((control.brightness() - 600.0 * TimeOfDay::NIGHT_DAYLIGHT).abs() < 1.0e-3);
    }

    #[test]
    fn the_sun_rises_east_peaks_at_its_noon_and_sets_west() {
        let path = SunPath::through(Vec3::new(-0.35, 0.77, 0.53));
        let at = |hour| {
            TimeOfDay {
                hour,
                ..TimeOfDay::default()
            }
            .sun_direction(&path)
        };

        assert!(at(6.0).y.abs() < 1.0e-5);
        assert!((at(12.0) - path.noon).length() < 1.0e-5);
        assert!(at(18.0).y.abs() < 1.0e-5);
        assert!((at(6.0) + at(18.0)).length() < 1.0e-5);
        assert!(at(0.0).y < 0.0);
    }

    #[test]
    fn a_day_wraps_and_its_length_steps_through_the_settings() {
        let mut clock = TimeOfDay::default();
        clock.advance(clock.day_length * 0.75);
        assert!((clock.hour - 4.0).abs() < 1.0e-3);

        clock.change_day_length(true);
        assert_eq!(clock.day_length, 600.0);
        for _ in 0..10 {
            clock.change_day_length(false);
        }
        assert_eq!(clock.day_length, 60.0);
    }

    #[test]
    fn a_sky_body_stops_at_the_wall_of_the_sky_room() {
        let bounds = (
            Vec3::new(-300.0, -100.0, -300.0),
            Vec3::new(300.0, 400.0, 300.0),
        );
        let distance =
            |direction: Vec3| crate::presentation::sky_wall_distance(bounds, direction.normalize());

        // Straight up leaves through the ceiling, not the far corner.
        assert!((distance(Vec3::Y) - 400.0).abs() < 1.0e-3);
        assert!((distance(Vec3::X) - 300.0).abs() < 1.0e-3);
        assert!((distance(Vec3::NEG_Y) - 100.0).abs() < 1.0e-3);
        // A diagonal leaves through whichever wall it reaches first.
        let diagonal = distance(Vec3::new(1.0, 1.0, 0.0));
        assert!(diagonal * std::f32::consts::FRAC_1_SQRT_2 <= 300.001);
    }

    #[test]
    fn a_colour_modifier_paints_the_material_it_wraps() {
        let materials = HashMap::from([
            (
                "sky.background".into(),
                MaterialGeometry {
                    texture: Some("sky.white".into()),
                    tint: Some([0.32, 0.35, 0.53, 1.0]),
                    ..default()
                },
            ),
            (
                "sky.white".into(),
                MaterialGeometry {
                    texture: Some("sky.white".into()),
                    ..default()
                },
            ),
        ]);
        let resolved =
            resolve_texture_from(&materials, |name| name == "sky.white", "sky.background").unwrap();

        assert_eq!(resolved.tint, [0.32, 0.35, 0.53, 1.0]);
    }

    #[test]
    fn dummy_texture_modify_info_does_not_override_materials() {
        let property = PropertyDump {
            name: "TexModifyInfo".into(),
            property_type: "TextureModifyinfo".into(),
            array_index: 0,
            size: 0,
            value: r#"{"AlphaOp":"1","bAlphaBlend":"true","bDummy":"true","bTwoSide":"true","bUseModify":"true"}"#
                .into(),
            raw: String::new(),
            wire_header: String::new(),
        };
        assert_eq!(
            texture_modify_override(&[property]),
            MaterialOverride::default()
        );
    }

    #[test]
    fn player_starts_sort_by_numeric_suffix() {
        let mut names = ["PlayerStart12", "PlayerStart2", "PlayerStart0"];
        names.sort_by_key(|name| player_start_number(name));
        assert_eq!(names, ["PlayerStart0", "PlayerStart2", "PlayerStart12"]);
    }

    #[test]
    fn zone_environment_maps_ambient_light_and_distance_fog() {
        let property = |name: &str, value: &str, raw: &str| PropertyDump {
            name: name.into(),
            property_type: String::new(),
            array_index: 0,
            size: raw.len() / 2,
            value: value.into(),
            raw: raw.into(),
            wire_header: String::new(),
        };
        let environment = scene_environment(&[
            property("AmbientVector", "0.1,0.2,0.3", ""),
            property("AmbientBrightness", "12", "0c"),
            property("bDistanceFog", "true", ""),
            property("DistanceFogColor", "", "102030ff"),
            property("DistanceFogStart", "128", ""),
            property("DistanceFogEnd", "1280", ""),
        ]);

        assert_eq!(environment.ambient_brightness, 120.0);
        let ambient = environment.ambient_color.to_srgba();
        assert!((ambient.red - 1.0 / 3.0).abs() < 1.0e-6);
        assert!((ambient.green - 2.0 / 3.0).abs() < 1.0e-6);
        assert!((ambient.blue - 1.0).abs() < 1.0e-6);
        let fog = environment.fog.unwrap();
        assert_eq!((fog.start, fog.end), (1.0, 10.0));
        assert_eq!(fog.color, Color::srgba_u8(0x30, 0x20, 0x10, 0xff));
    }

    #[test]
    fn sky_zone_without_lighting_does_not_reset_the_level_environment() {
        let property = |name: &str, value: &str| PropertyDump {
            name: name.into(),
            property_type: String::new(),
            array_index: 0,
            size: 0,
            value: value.into(),
            raw: String::new(),
            wire_header: String::new(),
        };
        let mut environment = SceneEnvironment::default();
        merge_scene_environment(
            &mut environment,
            &[
                property("AmbientVector", "0.45,0.45,0.45"),
                property("AmbientBrightness", "80"),
                property("bDistanceFog", "true"),
                property("DistanceFogEnd", "15000"),
            ],
        );
        merge_scene_environment(&mut environment, &[]);

        assert_eq!(environment.ambient_brightness, 800.0);
        assert_eq!(environment.ambient_color, Color::srgb(1.0, 1.0, 1.0));
        assert!(environment.fog.is_some());
    }

    #[test]
    fn lineage_movable_sun_is_a_directional_light() {
        let export = ExportDump {
            index: 0,
            reference: 1,
            name: "NMovableSunLight0".into(),
            full_name: "NMovableSunLight0".into(),
            class: "Engine.NMovableSunLight".into(),
            super_class: String::new(),
            outer: 0,
            flags: String::new(),
            size: 0,
            offset: 0,
            properties: Vec::new(),
            stack: None,
            native_object: None,
            unread_data: String::new(),
            raw_data: None,
        };

        assert!(is_scene_light_class(&export.class));
        assert!(matches!(scene_light(&export).kind, SceneLightKind::Sun));
    }

    #[test]
    fn missing_zone_colors_do_not_add_a_blue_cast() {
        let fog_enabled = PropertyDump {
            name: "bDistanceFog".into(),
            property_type: "Bool".into(),
            array_index: 0,
            size: 0,
            value: "true".into(),
            raw: String::new(),
            wire_header: String::new(),
        };
        let environment = scene_environment(&[fog_enabled]);
        assert_eq!(environment.ambient_color, Color::srgb(1.0, 1.0, 1.0));
        assert_eq!(
            environment.fog.unwrap().color,
            Color::srgb(0.38, 0.38, 0.38)
        );
    }

    #[test]
    fn projector_actor_maps_to_a_clustered_decal_volume() {
        let property = |name: &str, value: &str| PropertyDump {
            name: name.into(),
            property_type: String::new(),
            array_index: 0,
            size: 0,
            value: value.into(),
            raw: String::new(),
            wire_header: String::new(),
        };
        let export = ExportDump {
            index: 0,
            reference: 1,
            name: "Projector0".into(),
            full_name: "Projector0".into(),
            class: "Engine.Projector".into(),
            super_class: String::new(),
            outer: 0,
            flags: String::new(),
            size: 0,
            offset: 0,
            properties: vec![
                property("ProjTexture", "Effects.footprint"),
                property("Location", "128,256,384"),
                property("Rotation", "0,0,0"),
                property("FOV", "60"),
                property("MaxTraceDistance", "512"),
            ],
            stack: None,
            native_object: None,
            unread_data: String::new(),
            raw_data: None,
        };
        let decal = scene_decal(&export).unwrap();

        assert_eq!(decal.material, "Effects.footprint");
        assert_eq!(decal.location, Vec3::new(128.0, 384.0, -256.0));
        let expected_width = 2.0 * 512.0 * (30.0_f32.to_radians()).tan();
        assert!((decal.scale.x - expected_width).abs() < 0.001);
        assert!((decal.scale.y - expected_width).abs() < 0.001);
        assert_eq!(decal.scale.z, 512.0);

        let transform = projector_transform(&decal, Vec3::ZERO);
        let projection_direction = transform.rotation * Vec3::NEG_Z;
        assert!(projection_direction.distance(Vec3::X) < 0.000_001);
        assert!(transform.translation.distance(Vec3::new(3.0, 3.0, -2.0)) < 0.000_001);
    }

    #[test]
    fn opaque_projected_light_masks_are_not_rendered_as_base_color_decals() {
        assert!(!has_alpha_channel(&TextureGeometry {
            width: 1,
            height: 1,
            rgba: vec![35, 35, 35, 255],
        }));
        assert!(has_alpha_channel(&TextureGeometry {
            width: 1,
            height: 1,
            rgba: vec![255, 255, 255, 127],
        }));
    }

    #[test]
    fn camera_readout_uses_absolute_unreal_coordinates() {
        let coordinates = camera_unreal_coordinates(
            Vec3::new(2.0, 3.0, -4.0),
            Vec3::new(1_000.0, 2_000.0, -3_000.0),
        );
        assert_eq!(coordinates, Vec3::new(1_256.0, 3_512.0, 2_384.0));
    }

    #[test]
    fn selection_inspector_identifies_actor_object_material_and_location() {
        let description = selection_description(&SelectableObject {
            actor: "StaticMeshActor529".into(),
            object: "SSQ_dungeon_S.Broken_SSQ.fx_redcolor_water02".into(),
            material: Some("FX_E_T.WaterSurfaceShaderSet.ssq_water_01sh".into()),
            unreal_location: Vec3::new(137_023.0, 79_682.0, -3_736.0),
        });
        assert!(description.contains("Actor: StaticMeshActor529"));
        assert!(description.contains("Object: SSQ_dungeon_S.Broken_SSQ.fx_redcolor_water02"));
        assert!(description.contains("Material: FX_E_T.WaterSurfaceShaderSet.ssq_water_01sh"));
        assert!(description.contains("Location: X 137023  Y 79682  Z -3736"));
    }

    #[test]
    fn zone_portal_helper_surfaces_are_not_rendered() {
        assert!(is_zone_portal_material(Some("L2_ETC_Textures.ZonePortal")));
        assert!(!is_zone_portal_material(Some(
            "SSQ_dungeon_T.SSQ_Original.SSQ_ent09"
        )));
        assert!(!is_zone_portal_material(None));
    }

    #[test]
    fn invisible_bsp_portal_faces_are_not_rendered() {
        assert!(!should_render_bsp_surface(
            0x0400_0109,
            Some("SSQ_dungeon_T.SSQ_Original.SSQ_ground01")
        ));
        assert!(should_render_bsp_surface(
            0x0000_8108,
            Some("FX_E_T.WaterSurfaceShaderSet.ssq_water_01sh")
        ));
        // A fake backdrop is where the sky zone shows through.
        assert!(!should_render_bsp_surface(
            0x0000_0080,
            Some("sky.backdrop")
        ));
        assert!(should_render_bsp_surface(
            0x0040_a000,
            Some("SSQ_dungeon_T.SSQ_Original.SSQ_caveb")
        ));
    }

    #[test]
    fn selectable_meshes_retain_cpu_data_for_ray_casting() {
        let usage = selectable_mesh_usage();
        assert!(usage.contains(RenderAssetUsages::MAIN_WORLD));
        assert!(usage.contains(RenderAssetUsages::RENDER_WORLD));
    }

    #[test]
    fn repeated_selection_cycles_through_overlapping_hits() {
        assert_eq!(selection_hit_index(0, 3, false, false), 0);
        assert_eq!(selection_hit_index(0, 3, true, false), 1);
        assert_eq!(selection_hit_index(2, 3, true, false), 0);
        assert_eq!(selection_hit_index(0, 3, true, true), 2);
        assert_eq!(selection_hit_index(0, 3, false, true), 2);
    }

    #[test]
    fn lighting_controls_adjust_in_quarter_steps_and_stay_bounded() {
        assert_eq!(lighting_multiplier(1.0, 1), 1.25);
        assert_eq!(lighting_multiplier(1.25, -1), 1.0);
        assert_eq!(lighting_multiplier(64.0, 1), 64.0);
        assert_eq!(lighting_multiplier(0.05, -1), 0.05);
    }

    #[test]
    fn mirrored_meshes_reverse_their_culled_face() {
        assert_eq!(material_cull_mode(false, false), Some(Face::Front));
        assert_eq!(material_cull_mode(false, true), Some(Face::Back));
        assert_eq!(material_cull_mode(true, true), None);
    }

    #[test]
    #[ignore = "requires LINEAGE2_CLIENT to point to a complete client"]
    fn loads_external_meshes_from_lineage_client() {
        let client = PathBuf::from(
            std::env::var_os("LINEAGE2_CLIENT").expect("LINEAGE2_CLIENT is not configured"),
        );
        let (scene, report) = load_scene(&client.join("Maps/26_13.unr"), &[]).unwrap();
        report_scene(&scene, &report);
        assert!(report.loaded_packages > 0);
        assert!(report.loaded_material_packages > 0);
        assert!(report.loaded_textures > 0);
        assert_eq!(
            scene
                .terrain
                .as_ref()
                .map(|terrain| terrain.positions.len()),
            Some(256 * 256)
        );
        assert!(scene.textures.contains_key("brushis.groundtex"));
        assert!(scene.materials.contains_key("brushis.sh11"));
        assert_eq!(
            scene
                .bsp
                .iter()
                .flat_map(|geometry| &geometry.parts)
                .map(|part| part.indices.len() / 3)
                .sum::<usize>(),
            4
        );
        assert!(scene.resolved_instance_count() > 0);
    }

    #[test]
    #[ignore = "requires LINEAGE2_CLIENT to point to a complete client"]
    fn loads_textures_embedded_in_external_static_mesh_packages() {
        let client = PathBuf::from(
            std::env::var_os("LINEAGE2_CLIENT").expect("LINEAGE2_CLIENT is not configured"),
        );
        let (scene, _) = load_scene(&client.join("Maps/17_25.unr"), &[]).unwrap();
        let actor = scene
            .instances
            .iter()
            .find(|instance| instance.name == "StaticMeshActor140")
            .expect("lighthouse actor was not loaded");
        let mesh = scene
            .meshes
            .get(&actor.mesh.to_ascii_lowercase())
            .expect("lighthouse mesh was not resolved");

        assert!(mesh.parts.iter().any(|part| {
            part.material
                .as_deref()
                .is_some_and(|material| material.starts_with("sp_lighthouse."))
        }));
        assert!(
            scene
                .textures
                .contains_key("sp_lighthouse.sp_lighthouse009"),
            "the texture embedded beside the lighthouse mesh was not loaded"
        );
    }

    #[test]
    #[ignore = "requires LINEAGE2_CLIENT to point to a complete client"]
    fn maps_lineage_projectors_onto_their_trace_axis() {
        let client = PathBuf::from(
            std::env::var_os("LINEAGE2_CLIENT").expect("LINEAGE2_CLIENT is not configured"),
        );
        let (scene, _) = load_scene(&client.join("Maps/24_20.unr"), &[]).unwrap();

        let terrain = scene.terrain.as_ref().expect("24_20 has terrain");
        let visible_indices = terrain_indices(
            terrain.width,
            terrain.height,
            &terrain.quad_visibility,
            &terrain.edge_turns,
        );
        assert!(
            visible_indices.len() < (terrain.width - 1) * (terrain.height - 1) * 6,
            "terrain={}x{}, visibility={} bytes, indices={}",
            terrain.width,
            terrain.height,
            terrain.quad_visibility.len(),
            visible_indices.len()
        );
        for actor in ["StaticMeshActor529", "StaticMeshActor1113"] {
            let instance = scene
                .instances
                .iter()
                .find(|instance| instance.name == actor)
                .unwrap_or_else(|| panic!("{actor} was not loaded"));
            assert_eq!(instance.material_override, MaterialOverride::default());
        }
        assert_eq!(scene.player_starts[0].name, "PlayerStart0");
        assert_eq!(scene.player_starts.len(), 4);

        assert_eq!(scene.decals.len(), 23);
        let projector = scene
            .decals
            .iter()
            .find(|decal| decal.name == "Projector6")
            .unwrap();
        let texture = scene
            .textures
            .get(&projector.material.to_ascii_lowercase())
            .expect("projector texture was not resolved");
        assert!(!has_alpha_channel(texture));
        let transform = projector_transform(projector, scene.origin);
        let direction = transform.rotation * Vec3::NEG_Z;
        assert!(direction.y < -0.999);
        assert!(transform.translation.y < (projector.location.y - scene.origin.y) / 128.0);
        assert!((projector.scale.x - 2624.659).abs() < 0.1);
    }

    #[test]
    #[ignore = "requires LINEAGE2_CLIENT to point to a complete client"]
    fn builds_lineage_skeletal_mesh_rest_pose() {
        let client = PathBuf::from(
            std::env::var_os("LINEAGE2_CLIENT").expect("LINEAGE2_CLIENT is not configured"),
        );
        let package = Package::read(&client.join("Animations/LineageMonsters6.ukx")).unwrap();
        let dump = package.dump();
        let animations = dump
            .exports
            .iter()
            .filter_map(|export| match &export.native_object {
                Some(NativeObject::MeshAnimation(animation)) => Some((
                    export.full_name.to_ascii_lowercase(),
                    animation_geometry(animation.clone()),
                )),
                _ => None,
            })
            .collect::<HashMap<_, _>>();
        let geometry = dump
            .exports
            .into_iter()
            .find_map(|export| match export.native_object {
                Some(NativeObject::SkeletalMesh(mesh)) => Some(skeletal_mesh_geometry(*mesh)),
                _ => None,
            })
            .expect("package has no decoded skeletal mesh");

        assert!(!geometry.positions.is_empty());
        assert!(geometry.parts.iter().any(|part| !part.indices.is_empty()));
        let skin = geometry.skin.expect("skeletal mesh has no skin data");
        assert_eq!(skin.joint_indices.len(), geometry.positions.len());
        assert!(
            skin.joint_indices
                .iter()
                .flatten()
                .all(|joint| usize::from(*joint) < skin.bones.len())
        );
        let animation = animations
            .get(&skin.animation)
            .expect("mesh animation reference was not resolved");
        assert!(animation.duration > 0.0);
        assert!(
            animation
                .tracks
                .iter()
                .flatten()
                .any(|track| track.rotations.len() > 1)
        );
    }
}
