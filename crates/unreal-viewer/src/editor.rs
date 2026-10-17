//! In-viewer map editing: select a placed mesh actor, move, turn or drop it
//! with the editor panel or by dragging, undo, and save the edited placements
//! back into the map package.

use bevy::camera::primitives::Aabb;
use bevy::input::ButtonState;
use bevy::input::keyboard::{Key, KeyboardInput};
use bevy::input::mouse::MouseScrollUnit;
use unreal_package::unreal::{ActorAddition, PropertyEdit, reencrypt_like};

use super::*;

/// Rotator units in one full turn.
const ROTATOR_UNITS: i32 = 65_536;
/// Move steps offered by the panel, in Unreal units.
const STEP_SIZES: [f32; 4] = [1.0, 16.0, 64.0, 256.0];
/// Most catalog entries the browser lists at once; the search narrows them.
const BROWSER_LIMIT: usize = 250;
/// How far ahead of the camera an object lands when nothing is in view, in
/// viewer metres.
const PLACE_DISTANCE: f32 = 10.0;
/// How far the cursor moves before a press on the selection becomes a drag.
const DRAG_THRESHOLD: f32 = 4.0;

pub(super) struct MapEditorPlugin;

impl Plugin for MapEditorPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Startup,
            (
                spawn_editor_readout,
                spawn_editor_panel,
                spawn_toolbar,
                spawn_object_browser,
            ),
        )
            .add_systems(
                Update,
                (
                    track_panel_pointer
                        .before(drag_selected_actor)
                        .before(runtime::select_rendered_object),
                    drag_selected_actor.before(runtime::select_rendered_object),
                    finish_drag.after(runtime::select_rendered_object),
                    press_editor_buttons,
                    scroll_selected_height,
                    editor_shortcuts,
                    show_editor_panel.after(press_editor_buttons),
                    type_in_browser,
                    scroll_browser,
                    refresh_object_browser
                        .after(press_editor_buttons)
                        .after(type_in_browser),
                    draw_selection_axes,
                    update_editor_readout,
                ),
            );
    }
}

/// A placed actor's editable transform, in Unreal units.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Placement {
    location: Vec3,
    rotation: IVec3,
}

struct EditableActor {
    name: String,
    mesh: String,
    original: Placement,
    col_location: Option<Vec3>,
}

/// A mesh the object browser offers. It is placed by copying an actor that
/// already shows it, so the copy has the mesh's materials and the map's
/// level, zone and display settings.
struct CatalogEntry {
    mesh: String,
    template: usize,
    uses: usize,
}

fn catalog_from(instances: &[MeshInstance]) -> Vec<CatalogEntry> {
    let mut catalog: BTreeMap<String, CatalogEntry> = BTreeMap::new();
    for instance in instances {
        let Some(source) = instance.source.filter(|source| source.static_mesh_actor) else {
            continue;
        };
        catalog
            .entry(instance.mesh.to_ascii_lowercase())
            .or_insert_with(|| CatalogEntry {
                mesh: instance.mesh.clone(),
                template: source.export,
                uses: 0,
            })
            .uses += 1;
    }
    catalog.into_values().collect()
}

/// An actor created in the viewer, written to the package on save.
#[derive(Clone, Debug, PartialEq)]
struct Addition {
    /// The export index the actor will have once added.
    export: usize,
    /// The actor it was copied from.
    template: usize,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum UndoStep {
    Moved(usize, Placement),
    Added(usize),
}

struct Drag {
    export: usize,
    start_cursor: Vec2,
    plane_origin: Vec3,
    plane_normal: Vec3,
    start_hit: Vec3,
    vertical: bool,
    start: Placement,
    /// Height added with the mouse wheel while the actor is held, in Unreal
    /// units.
    height: f32,
    active: bool,
}

#[derive(Resource)]
pub(super) struct MapEditor {
    path: PathBuf,
    /// The package exactly as loaded; every save rebuilds from it so repeated
    /// saves do not accumulate rewritten exports.
    wire: Vec<u8>,
    actors: HashMap<usize, EditableActor>,
    edits: BTreeMap<usize, Placement>,
    saved: BTreeMap<usize, Placement>,
    /// Exports the loaded package has; added actors are numbered after them.
    base_exports: usize,
    additions: Vec<Addition>,
    saved_additions: Vec<Addition>,
    undo: Vec<UndoStep>,
    pub(super) selected: Option<usize>,
    drag: Option<Drag>,
    /// Move step of the panel buttons, in Unreal units.
    step: f32,
    pointer_over_panel: bool,
    pointer_over_browser: bool,
    catalog: Vec<CatalogEntry>,
    browser_open: bool,
    search: String,
    status: String,
}

impl MapEditor {
    pub(super) fn new(path: &Path, wire: Vec<u8>, instances: &[MeshInstance]) -> Self {
        let actors: HashMap<usize, EditableActor> = instances
            .iter()
            .filter_map(|instance| {
                let source = instance.source?;
                Some((
                    source.export,
                    EditableActor {
                        name: instance.name.clone(),
                        mesh: instance.mesh.clone(),
                        original: Placement {
                            location: source.location,
                            rotation: source.rotation,
                        },
                        col_location: source.col_location,
                    },
                ))
            })
            .collect();
        let base_exports = Package::from_wire_bytes(path, wire.clone())
            .map(|package| package.export_count())
            .unwrap_or_else(|_| actors.keys().max().map_or(0, |export| export + 1));
        Self {
            path: path.to_path_buf(),
            wire,
            actors,
            edits: BTreeMap::new(),
            saved: BTreeMap::new(),
            base_exports,
            additions: Vec::new(),
            saved_additions: Vec::new(),
            undo: Vec::new(),
            selected: None,
            drag: None,
            step: STEP_SIZES[1],
            pointer_over_panel: false,
            pointer_over_browser: false,
            catalog: catalog_from(instances),
            browser_open: false,
            search: String::new(),
            status: String::new(),
        }
    }

    /// Whether keys go to the object browser's search rather than the camera.
    pub(super) fn typing_in_browser(&self) -> bool {
        self.browser_open && self.pointer_over_browser
    }

    /// Catalog entries whose mesh name contains every word of the search.
    fn matching_catalog(&self) -> Vec<usize> {
        let terms: Vec<String> = self
            .search
            .split_whitespace()
            .map(str::to_ascii_lowercase)
            .collect();
        self.catalog
            .iter()
            .enumerate()
            .filter(|(_, entry)| {
                let mesh = entry.mesh.to_ascii_lowercase();
                terms.iter().all(|term| mesh.contains(term))
            })
            .map(|(index, _)| index)
            .collect()
    }

    /// Whether a click belongs to the editor panel rather than the scene.
    pub(super) fn pointer_over_panel(&self) -> bool {
        self.pointer_over_panel
    }

    pub(super) fn is_dragging(&self) -> bool {
        self.drag.as_ref().is_some_and(|drag| drag.active)
    }

    fn placement(&self, export: usize) -> Option<Placement> {
        self.edits
            .get(&export)
            .copied()
            .or_else(|| self.actors.get(&export).map(|actor| actor.original))
    }

    fn set_placement(&mut self, export: usize, placement: Placement) {
        if self
            .actors
            .get(&export)
            .is_some_and(|actor| actor.original == placement)
        {
            self.edits.remove(&export);
        } else {
            self.edits.insert(export, placement);
        }
    }

    fn unsaved_changes(&self) -> usize {
        let exports: BTreeSet<_> = self.edits.keys().chain(self.saved.keys()).collect();
        let moved = exports
            .into_iter()
            .filter(|export| self.edits.get(export) != self.saved.get(export))
            .count();
        let unchanged_additions = self
            .additions
            .iter()
            .zip(&self.saved_additions)
            .take_while(|(addition, saved)| addition == saved)
            .count();
        moved + self.additions.len() + self.saved_additions.len() - 2 * unchanged_additions
    }

    /// Records a copy of `template` at `placement` and returns the export
    /// index it will be saved as.
    fn add_copy(&mut self, template: usize, placement: Placement) -> Option<usize> {
        let name = format!("Copy of {}", self.actors.get(&template)?.name);
        self.add_placed(template, placement, name)
    }

    /// Records a new actor cloned from `template`, shown as `name` until saved.
    fn add_placed(&mut self, template: usize, placement: Placement, name: String) -> Option<usize> {
        let source = self.actors.get(&template)?;
        let export = self.base_exports + self.additions.len();
        let actor = EditableActor {
            name,
            mesh: source.mesh.clone(),
            original: placement,
            col_location: None,
        };
        self.actors.insert(export, actor);
        self.additions.push(Addition { export, template });
        self.undo.push(UndoStep::Added(export));
        Some(export)
    }

    /// Forgets the most recent copy, which undo always reaches last-in first.
    fn remove_addition(&mut self, export: usize) {
        if self.additions.last().is_some_and(|addition| addition.export == export) {
            self.additions.pop();
            self.actors.remove(&export);
            self.edits.remove(&export);
            if self.selected == Some(export) {
                self.selected = None;
            }
        }
    }

    fn property_edits(&self) -> Vec<PropertyEdit> {
        let mut edits = Vec::new();
        // Added actors are written whole with their current placement.
        for (&export, placement) in self.edits.range(..self.base_exports) {
            let actor = &self.actors[&export];
            let mut edit = |name: &str, property_type: &str, value: String| {
                edits.push(PropertyEdit {
                    export,
                    name: name.into(),
                    property_type: property_type.into(),
                    value,
                });
            };
            if placement.location != actor.original.location {
                edit("Location", "Vector", vector_value(placement.location));
                // Lineage II keeps a separate collision location; it follows
                // the actor so the offset between the two is preserved.
                if let Some(col_location) = actor.col_location {
                    let moved = col_location + placement.location - actor.original.location;
                    edit("ColLocation", "Vector", vector_value(moved));
                }
            }
            if placement.rotation != actor.original.rotation {
                let rotation = placement.rotation;
                edit(
                    "Rotation",
                    "Rotator",
                    format!("{},{},{}", rotation.x, rotation.y, rotation.z),
                );
            }
        }
        edits
    }

    /// Writes the edited map over the loaded file, keeping the first version
    /// that was ever overwritten next to it as `<map>.bak`.
    fn save(&mut self) -> Result<PathBuf, String> {
        let mut package = Package::from_wire_bytes(&self.path, self.wire.clone())?;
        for addition in &self.additions {
            let placement = self
                .placement(addition.export)
                .ok_or("an added actor has no placement")?;
            let rotation = placement.rotation;
            let (bytes, export) = package.add_actor(&ActorAddition {
                template: addition.template,
                static_mesh: None,
                location: vector_value(placement.location),
                rotation: format!("{},{},{}", rotation.x, rotation.y, rotation.z),
            })?;
            if export != addition.export {
                return Err(format!(
                    "added actor became export {export}, expected {}",
                    addition.export
                ));
            }
            package = Package::from_wire_bytes(&self.path, bytes)?;
        }
        let plain = package.rebuild_with_property_edits(&self.property_edits())?;
        let filename = self
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        let bytes = reencrypt_like(&self.wire, plain, filename)?;
        let backup = backup_path(&self.path);
        if !backup.exists() {
            fs::copy(&self.path, &backup)
                .map_err(|error| format!("cannot back up to {}: {error}", backup.display()))?;
        }
        let temporary = with_suffix(&self.path, ".saving");
        fs::write(&temporary, bytes)
            .map_err(|error| format!("cannot write {}: {error}", temporary.display()))?;
        fs::rename(&temporary, &self.path)
            .map_err(|error| format!("cannot replace {}: {error}", self.path.display()))?;
        self.saved = self.edits.clone();
        self.saved_additions = self.additions.clone();
        Ok(backup)
    }
}

fn vector_value(value: Vec3) -> String {
    format!("{},{},{}", value.x, value.y, value.z)
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

fn backup_path(path: &Path) -> PathBuf {
    with_suffix(path, ".bak")
}

fn viewer_translation(location: Vec3, scene_origin: Vec3) -> Vec3 {
    (scene::unreal_direction_vec(location) - scene_origin) / UNREAL_UNITS_PER_METER
}

fn viewer_rotation(rotation: IVec3) -> Quat {
    unreal_rotation(rotation.as_vec3() * std::f32::consts::TAU / ROTATOR_UNITS as f32)
}

/// Converts a displacement in viewer metres to Unreal units.
fn unreal_delta(delta: Vec3) -> Vec3 {
    scene::unreal_direction_vec(delta) * UNREAL_UNITS_PER_METER
}

fn menus_open(view: &SelectionView) -> bool {
    view.teleport_menu.open
        || view.world_map.open
        || view.settings_menu.open
        || view.game_event_menu.open
}

fn apply_placement(
    parts: &mut Query<(Entity, &mut Transform, &mut SelectableObject)>,
    scene_origin: Vec3,
    export: usize,
    placement: Placement,
) {
    let translation = viewer_translation(placement.location, scene_origin);
    let rotation = viewer_rotation(placement.rotation);
    for (_, mut transform, mut object) in parts.iter_mut() {
        if object.export == Some(export) {
            transform.translation = translation;
            transform.rotation = rotation;
            object.unreal_location = placement.location;
        }
    }
}

fn drag_selected_actor(
    mut ray_cast: MeshRayCast,
    view: SelectionView,
    scene: Res<MapScene>,
    mut editor: ResMut<MapEditor>,
    mut parts: Query<(Entity, &mut Transform, &mut SelectableObject)>,
) {
    if menus_open(&view) {
        editor.drag = None;
        return;
    }
    let Some(cursor) = view.window.cursor_position() else {
        return;
    };
    let (camera, camera_transform) = *view.camera;
    let Ok(ray) = camera.viewport_to_world(camera_transform, cursor) else {
        return;
    };
    if view.buttons.just_pressed(MouseButton::Left) {
        editor.drag = None;
        if editor.pointer_over_panel {
            return;
        }
        let Some(selected) = editor.selected else {
            return;
        };
        let top = {
            let selectable = |entity| parts.contains(entity);
            let settings = MeshRayCastSettings::default().with_filter(&selectable);
            ray_cast.cast_ray(ray, &settings).first().map(|(entity, _)| *entity)
        };
        let Some((_, transform, object)) = top.and_then(|entity| parts.get(entity).ok()) else {
            return;
        };
        if object.export != Some(selected) {
            return;
        }
        let vertical = view.keys.pressed(KeyCode::AltLeft) || view.keys.pressed(KeyCode::AltRight);
        let plane_normal = if vertical {
            let forward = camera_transform.forward();
            Vec3::new(forward.x, 0.0, forward.z).normalize_or(Vec3::Z)
        } else {
            Vec3::Y
        };
        let plane_origin = transform.translation;
        let (Some(distance), Some(start)) = (
            ray.intersect_plane(plane_origin, InfinitePlane3d::new(plane_normal)),
            editor.placement(selected),
        ) else {
            return;
        };
        editor.drag = Some(Drag {
            export: selected,
            start_cursor: cursor,
            plane_origin,
            plane_normal,
            start_hit: ray.get_point(distance),
            vertical,
            start,
            height: 0.0,
            active: false,
        });
        return;
    }
    if !view.buttons.pressed(MouseButton::Left) {
        return;
    }
    let Some(drag) = editor.drag.as_mut() else {
        return;
    };
    if !drag.active && drag.start_cursor.distance(cursor) < DRAG_THRESHOLD {
        return;
    }
    drag.active = true;
    let Some(distance) =
        ray.intersect_plane(drag.plane_origin, InfinitePlane3d::new(drag.plane_normal))
    else {
        return;
    };
    let delta = unreal_delta(ray.get_point(distance) - drag.start_hit);
    let mut location = drag.start.location;
    location.z += drag.height;
    if drag.vertical {
        location.z = (location.z + delta.z).round();
    } else {
        location.x = (location.x + delta.x).round();
        location.y = (location.y + delta.y).round();
    }
    let placement = Placement {
        location,
        rotation: drag.start.rotation,
    };
    let export = drag.export;
    editor.set_placement(export, placement);
    apply_placement(&mut parts, scene.origin, export, placement);
}

fn finish_drag(buttons: Res<ButtonInput<MouseButton>>, mut editor: ResMut<MapEditor>) {
    if !buttons.just_released(MouseButton::Left) {
        return;
    }
    if let Some(drag) = editor.drag.take().filter(|drag| drag.active) {
        editor.undo.push(UndoStep::Moved(drag.export, drag.start));
    }
}

/// Seconds between wheel notches that still count as the same height change,
/// so one spin of the wheel undoes as one step.
const WHEEL_GESTURE_GAP: f32 = 0.6;
/// Pixels of touchpad scrolling that count as one wheel notch.
const PIXELS_PER_NOTCH: f32 = 40.0;

#[derive(Default)]
struct WheelGesture {
    export: Option<usize>,
    pending: f32,
    last: f32,
}

/// Raises or lowers the selection by one step per mouse wheel notch.
fn scroll_selected_height(
    view: SelectionView,
    scroll: Res<AccumulatedMouseScroll>,
    time: Res<Time>,
    scene: Res<MapScene>,
    mut editor: ResMut<MapEditor>,
    mut parts: Query<(Entity, &mut Transform, &mut SelectableObject)>,
    mut gesture: Local<WheelGesture>,
) {
    let Some(export) = editor.selected else {
        return;
    };
    if scroll.delta.y == 0.0 || menus_open(&view) || editor.pointer_over_panel {
        return;
    }
    let notches = match scroll.unit {
        bevy::input::mouse::MouseScrollUnit::Line => scroll.delta.y,
        bevy::input::mouse::MouseScrollUnit::Pixel => scroll.delta.y / PIXELS_PER_NOTCH,
    };
    let now = time.elapsed_secs();
    let continuing =
        gesture.export == Some(export) && now - gesture.last <= WHEEL_GESTURE_GAP;
    if !continuing {
        gesture.pending = 0.0;
    }
    gesture.pending += notches;
    gesture.last = now;
    let whole = gesture.pending.trunc();
    if whole == 0.0 {
        return;
    }
    gesture.pending -= whole;
    let Some(before) = editor.placement(export) else {
        return;
    };
    let mut placement = before;
    let lift = whole * editor.step;
    placement.location.z += lift;
    if let Some(drag) = editor.drag.as_mut().filter(|drag| drag.export == export) {
        // Held: the drag keeps the height and undoes as one step on release.
        drag.height += lift;
        drag.active = true;
    } else if gesture.export != Some(export) || !continuing {
        editor.undo.push(UndoStep::Moved(export, before));
    }
    gesture.export = Some(export);
    editor.set_placement(export, placement);
    apply_placement(&mut parts, scene.origin, export, placement);
}

/// Keyboard chords kept alongside the panel buttons.
fn editor_shortcuts(
    mut commands: Commands,
    view: SelectionView,
    scene: Res<MapScene>,
    mut editor: ResMut<MapEditor>,
    mut parts: Query<(Entity, &mut Transform, &mut SelectableObject)>,
) {
    if menus_open(&view) || editor.is_dragging() {
        return;
    }
    let keys = &view.keys;
    if !(keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::ControlRight)) {
        return;
    }
    if keys.just_pressed(KeyCode::KeyZ) {
        undo(&mut commands, &mut editor, &mut parts, scene.origin);
    }
    if keys.just_pressed(KeyCode::KeyS) {
        save(&mut editor);
    }
}

fn undo(
    commands: &mut Commands,
    editor: &mut MapEditor,
    parts: &mut Query<(Entity, &mut Transform, &mut SelectableObject)>,
    scene_origin: Vec3,
) {
    match editor.undo.pop() {
        Some(UndoStep::Moved(export, placement)) => {
            editor.set_placement(export, placement);
            apply_placement(parts, scene_origin, export, placement);
            editor.selected = Some(export);
            editor.status = "Undone".into();
        }
        Some(UndoStep::Added(export)) => {
            for (entity, _, object) in parts.iter() {
                if object.export == Some(export) {
                    commands.entity(entity).despawn();
                }
            }
            editor.remove_addition(export);
            editor.status = "Removed the copy".into();
        }
        None => editor.status = "Nothing to undo".into(),
    }
}

/// Spawns a copy of the selected actor's surfaces beside it and selects it.
fn copy_selection(
    commands: &mut Commands,
    editor: &mut MapEditor,
    parts: &Query<(Entity, &mut Transform, &mut SelectableObject)>,
    highlight: &SelectionState,
    scene_origin: Vec3,
    offset: Vec3,
) {
    let Some(template) = editor.selected else {
        return;
    };
    let Some(mut placement) = editor.placement(template) else {
        return;
    };
    placement.location += offset;
    let Some(export) = editor.add_copy(template, placement) else {
        return;
    };
    spawn_copy(commands, editor, parts, highlight, scene_origin, template, export);
    let name = &editor.actors[&export].name;
    editor.status = format!("Copied; {name} is selected (saved with the map)");
}

/// Spawns the surfaces of `template` for the added actor `export` at its
/// placement and selects it.
fn spawn_copy(
    commands: &mut Commands,
    editor: &mut MapEditor,
    parts: &Query<(Entity, &mut Transform, &mut SelectableObject)>,
    highlight: &SelectionState,
    scene_origin: Vec3,
    template: usize,
    export: usize,
) {
    let Some(placement) = editor.placement(export) else {
        return;
    };
    let name = editor.actors[&export].name.clone();
    let translation = viewer_translation(placement.location, scene_origin);
    let rotation = viewer_rotation(placement.rotation);
    for (entity, transform, object) in parts.iter() {
        if object.export != Some(template) {
            continue;
        }
        let mut source = commands.entity(entity);
        let mut copy = source.clone_and_spawn_with_opt_out(|builder| {
            builder.deny::<SelectableObject>().linked_cloning(true);
        });
        copy.insert((
            Transform {
                translation,
                rotation,
                scale: transform.scale,
            },
            SelectableObject {
                actor: name.clone(),
                object: object.object.clone(),
                material: object.material.clone(),
                unreal_location: placement.location,
                export: Some(export),
            },
        ));
        // The selected surface wears the highlight; its copy gets the real
        // material back.
        if let Some((selected, material)) = &highlight.selected
            && *selected == entity
        {
            copy.insert(MeshMaterial3d(material.clone()));
        }
    }
    editor.selected = Some(export);
}

/// Where the centre of the view meets the scene, in Unreal coordinates, or a
/// point ahead of the camera when it looks at nothing.
fn placement_point(
    ray_cast: &mut MeshRayCast,
    parts: &Query<(Entity, &mut Transform, &mut SelectableObject)>,
    camera: &GlobalTransform,
    scene_origin: Vec3,
) -> Vec3 {
    let ray = Ray3d::new(camera.translation(), camera.forward());
    let surface = |entity| {
        parts
            .get(entity)
            .is_ok_and(|(_, _, object)| object.object != "Engine.Emitter")
    };
    let settings = MeshRayCastSettings::default().with_filter(&surface);
    let point = ray_cast
        .cast_ray(ray, &settings)
        .first()
        .map(|(_, hit)| hit.point)
        .unwrap_or_else(|| ray.get_point(PLACE_DISTANCE));
    camera_unreal_coordinates(point, scene_origin).round()
}

fn save(editor: &mut MapEditor) {
    editor.status = match editor.save() {
        Ok(backup) => format!(
            "Saved {} (original kept as {})",
            editor.path.display(),
            backup.display()
        ),
        Err(error) => format!("Save failed: {error}"),
    };
    eprintln!("viewer: {}", editor.status);
}

/// Moves or turns an actor as one undoable step.
fn change_placement(
    editor: &mut MapEditor,
    parts: &mut Query<(Entity, &mut Transform, &mut SelectableObject)>,
    scene_origin: Vec3,
    export: usize,
    placement: Placement,
) {
    let Some(before) = editor.placement(export) else {
        return;
    };
    if placement == before {
        return;
    }
    editor.undo.push(UndoStep::Moved(export, before));
    editor.set_placement(export, placement);
    apply_placement(parts, scene_origin, export, placement);
}

/// The Unreal axis, X or Y, closest to a horizontal viewer direction, so
/// camera-relative moves stay aligned with the map grid.
fn snapped_horizontal_axis(viewer_direction: Vec3) -> Vec3 {
    let unreal = scene::unreal_direction_vec(viewer_direction);
    if unreal.x.abs() >= unreal.y.abs() {
        Vec3::new(unreal.x.signum(), 0.0, 0.0)
    } else {
        Vec3::new(0.0, unreal.y.signum(), 0.0)
    }
}

fn nudge_direction(nudge: Nudge, camera: &GlobalTransform) -> Vec3 {
    match nudge {
        Nudge::Forward => snapped_horizontal_axis(*camera.forward()),
        Nudge::Back => -snapped_horizontal_axis(*camera.forward()),
        Nudge::Right => snapped_horizontal_axis(*camera.right()),
        Nudge::Left => -snapped_horizontal_axis(*camera.right()),
        Nudge::Up => Vec3::Z,
        Nudge::Down => Vec3::NEG_Z,
    }
}

fn rotated(placement: Placement, yaw: i32) -> Placement {
    let mut rotation = placement.rotation;
    rotation.y = (rotation.y + yaw).rem_euclid(ROTATOR_UNITS);
    Placement {
        rotation,
        ..placement
    }
}

/// How far the selected actor must move down, in Unreal units, to rest its
/// lowest point on the first surface below it.
fn drop_distance(
    ray_cast: &mut MeshRayCast,
    parts: &Query<(Entity, &mut Transform, &mut SelectableObject)>,
    bounds: &Query<(&Aabb, &GlobalTransform)>,
    export: usize,
) -> Option<f32> {
    let mut lowest = f32::MAX;
    let mut center = Vec3::ZERO;
    let mut count = 0.0;
    for (entity, _, object) in parts.iter() {
        if object.export != Some(export) {
            continue;
        }
        let Ok((aabb, transform)) = bounds.get(entity) else {
            continue;
        };
        let (middle, half) = (Vec3::from(aabb.center), Vec3::from(aabb.half_extents));
        for corner in 0..8 {
            let sign = Vec3::new(
                if corner & 1 == 0 { -1.0 } else { 1.0 },
                if corner & 2 == 0 { -1.0 } else { 1.0 },
                if corner & 4 == 0 { -1.0 } else { 1.0 },
            );
            let point = transform.transform_point(middle + half * sign);
            lowest = lowest.min(point.y);
            center += point;
            count += 1.0;
        }
    }
    if count == 0.0 {
        return None;
    }
    center /= count;
    // Start just above the base so the floor the actor sinks into is found.
    let origin = Vec3::new(center.x, lowest + 0.05, center.z);
    let floor = |entity| {
        parts.get(entity).is_ok_and(|(_, _, object)| {
            object.export != Some(export) && object.object != "Engine.Emitter"
        })
    };
    let settings = MeshRayCastSettings::default().with_filter(&floor);
    let (_, hit) = ray_cast
        .cast_ray(Ray3d::new(origin, Dir3::NEG_Y), &settings)
        .first()?;
    Some((lowest - hit.point.y) * UNREAL_UNITS_PER_METER)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Nudge {
    Forward,
    Back,
    Left,
    Right,
    Up,
    Down,
}

#[derive(Component, Clone, Copy, Debug, PartialEq)]
enum EditorAction {
    Move(Nudge),
    Rotate(i32),
    Step(f32),
    DropToFloor,
    Copy,
    ToggleBrowser,
    Place(usize),
    Undo,
    Save,
    Close,
}

#[derive(Component)]
struct EditorPanel;

#[derive(Component)]
struct EditorPanelText;

const BUTTON_COLOR: Color = Color::srgb(0.10, 0.13, 0.18);
const BUTTON_HOVER_COLOR: Color = Color::srgb(0.16, 0.21, 0.29);
const BUTTON_ACTIVE_COLOR: Color = Color::srgb(0.22, 0.38, 0.58);

fn spawn_editor_panel(mut commands: Commands) {
    let panel = commands
        .spawn((
            Name::new("Map editor panel"),
            Node {
                position_type: PositionType::Absolute,
                left: px(12),
                top: px(12),
                width: px(340),
                flex_direction: FlexDirection::Column,
                padding: UiRect::all(px(12)),
                row_gap: px(6),
                border_radius: BorderRadius::all(px(6)),
                ..default()
            },
            BackgroundColor(Color::srgba(0.015, 0.02, 0.03, 0.92)),
            Interaction::default(),
            Visibility::Hidden,
            EditorUi,
            EditorPanel,
        ))
        .id();
    commands.entity(panel).with_children(|parent| {
        parent.spawn((
            Text::new(""),
            TextFont {
                font_size: FontSize::Px(15.0),
                ..default()
            },
            TextColor(Color::WHITE),
            EditorPanelText,
        ));
        panel_label(parent, "Step (Unreal units)");
        panel_row(
            parent,
            STEP_SIZES.map(|step| (format!("{step:.0}"), EditorAction::Step(step))),
        );
        panel_label(parent, "Move (relative to the camera)");
        panel_row(
            parent,
            [
                ("Left".to_string(), EditorAction::Move(Nudge::Left)),
                ("Forward".to_string(), EditorAction::Move(Nudge::Forward)),
                ("Right".to_string(), EditorAction::Move(Nudge::Right)),
            ],
        );
        panel_row(
            parent,
            [
                ("Down".to_string(), EditorAction::Move(Nudge::Down)),
                ("Back".to_string(), EditorAction::Move(Nudge::Back)),
                ("Up".to_string(), EditorAction::Move(Nudge::Up)),
            ],
        );
        panel_row(
            parent,
            [
                ("Copy".to_string(), EditorAction::Copy),
                ("Drop to floor".to_string(), EditorAction::DropToFloor),
            ],
        );
        panel_label(parent, "Rotate");
        panel_row(
            parent,
            [
                ("-90".to_string(), EditorAction::Rotate(-ROTATOR_UNITS / 4)),
                ("-15".to_string(), EditorAction::Rotate(-ROTATOR_UNITS / 24)),
                ("+15".to_string(), EditorAction::Rotate(ROTATOR_UNITS / 24)),
                ("+90".to_string(), EditorAction::Rotate(ROTATOR_UNITS / 4)),
            ],
        );
        panel_row(
            parent,
            [
                ("Undo".to_string(), EditorAction::Undo),
                ("Save".to_string(), EditorAction::Save),
                ("Close".to_string(), EditorAction::Close),
            ],
        );
    });
}

fn panel_label(parent: &mut ChildSpawnerCommands, label: &str) {
    parent.spawn((
        Text::new(label),
        TextFont {
            font_size: FontSize::Px(13.0),
            ..default()
        },
        TextColor(Color::srgb(0.65, 0.7, 0.78)),
        Node {
            margin: UiRect::top(px(4)),
            ..default()
        },
    ));
}

fn panel_row<const N: usize>(
    parent: &mut ChildSpawnerCommands,
    buttons: [(String, EditorAction); N],
) {
    parent
        .spawn(Node {
            flex_direction: FlexDirection::Row,
            column_gap: px(6),
            ..default()
        })
        .with_children(|row| {
            for (label, action) in buttons {
                row.spawn((
                    Button,
                    Node {
                        flex_grow: 1.0,
                        flex_basis: px(0),
                        min_height: px(34),
                        justify_content: JustifyContent::Center,
                        align_items: AlignItems::Center,
                        border_radius: BorderRadius::all(px(4)),
                        ..default()
                    },
                    BackgroundColor(BUTTON_COLOR),
                    action,
                ))
                .with_children(|button| {
                    button.spawn((
                        Text::new(label),
                        TextFont {
                            font_size: FontSize::Px(16.0),
                            ..default()
                        },
                        TextColor(Color::WHITE),
                    ));
                });
            }
        });
}

/// Whether the pointer is over the panel, so clicks on it do not reach the
/// scene behind.
fn track_panel_pointer(
    roots: Query<(&Interaction, &Visibility, Has<ObjectBrowser>), With<EditorUi>>,
    buttons: Query<(&Interaction, Has<BrowserItem>), With<EditorAction>>,
    mut editor: ResMut<MapEditor>,
) {
    // A hovered button takes the pointer from the panel behind it; buttons in
    // hidden panels never hover.
    let hovered = |interaction: &Interaction| *interaction != Interaction::None;
    let mut over = false;
    let mut over_browser = false;
    for (interaction, visibility, browser) in &roots {
        if *visibility != Visibility::Hidden && hovered(interaction) {
            over = true;
            over_browser |= browser;
        }
    }
    for (interaction, in_browser) in &buttons {
        if hovered(interaction) {
            over = true;
            over_browser |= in_browser;
        }
    }
    if editor.pointer_over_panel != over {
        editor.pointer_over_panel = over;
    }
    if editor.pointer_over_browser != over_browser {
        editor.pointer_over_browser = over_browser;
    }
}

/// Every top-level editor UI node; the pointer over one belongs to the UI.
#[derive(Component)]
struct EditorUi;

#[derive(Component)]
struct ObjectBrowser;

#[derive(Component)]
struct BrowserTitle;

#[derive(Component)]
struct BrowserList;

/// A button inside the object browser.
#[derive(Component)]
struct BrowserItem;

fn spawn_toolbar(mut commands: Commands) {
    let toolbar = commands
        .spawn((
            Name::new("Map editor toolbar"),
            Node {
                position_type: PositionType::Absolute,
                left: px(370),
                top: px(12),
                width: px(330),
                padding: UiRect::all(px(6)),
                border_radius: BorderRadius::all(px(6)),
                ..default()
            },
            BackgroundColor(Color::srgba(0.015, 0.02, 0.03, 0.92)),
            Interaction::default(),
            EditorUi,
        ))
        .id();
    commands.entity(toolbar).with_children(|parent| {
        panel_row(
            parent,
            [
                ("Objects".to_string(), EditorAction::ToggleBrowser),
                ("Undo".to_string(), EditorAction::Undo),
                ("Save".to_string(), EditorAction::Save),
            ],
        );
    });
}

fn spawn_object_browser(mut commands: Commands) {
    let browser = commands
        .spawn((
            Name::new("Object browser"),
            Node {
                position_type: PositionType::Absolute,
                right: px(12),
                top: px(110),
                bottom: px(12),
                width: px(460),
                flex_direction: FlexDirection::Column,
                padding: UiRect::all(px(12)),
                row_gap: px(6),
                border_radius: BorderRadius::all(px(6)),
                ..default()
            },
            BackgroundColor(Color::srgba(0.015, 0.02, 0.03, 0.94)),
            Interaction::default(),
            Visibility::Hidden,
            EditorUi,
            ObjectBrowser,
        ))
        .id();
    commands.entity(browser).with_children(|parent| {
        parent.spawn((
            Text::new(""),
            TextFont {
                font_size: FontSize::Px(15.0),
                ..default()
            },
            TextColor(Color::WHITE),
            BrowserTitle,
        ));
        parent.spawn((
            Node {
                width: percent(100),
                flex_grow: 1.0,
                flex_direction: FlexDirection::Column,
                overflow: Overflow::scroll_y(),
                scrollbar_width: 12.0,
                row_gap: px(3),
                ..default()
            },
            ScrollPosition::default(),
            BrowserList,
        ));
        panel_row(parent, [("Close".to_string(), EditorAction::ToggleBrowser)]);
    });
}

/// Keys typed while the pointer is over the browser edit its search.
fn type_in_browser(mut keys: MessageReader<KeyboardInput>, mut editor: ResMut<MapEditor>) {
    let typing = editor.typing_in_browser();
    for key in keys.read() {
        if !typing || key.state != ButtonState::Pressed {
            continue;
        }
        match &key.logical_key {
            Key::Character(text) => {
                let text: String = text.chars().filter(|character| !character.is_control()).collect();
                editor.search.push_str(&text);
            }
            Key::Space => editor.search.push(' '),
            Key::Backspace => {
                editor.search.pop();
            }
            _ => {}
        }
    }
}

fn scroll_browser(
    scroll: Res<AccumulatedMouseScroll>,
    editor: Res<MapEditor>,
    mut list: Single<&mut ScrollPosition, With<BrowserList>>,
) {
    if !editor.browser_open || !editor.pointer_over_browser || scroll.delta.y == 0.0 {
        return;
    }
    let lines = match scroll.unit {
        MouseScrollUnit::Line => scroll.delta.y,
        MouseScrollUnit::Pixel => scroll.delta.y / PIXELS_PER_NOTCH,
    };
    list.y = (list.y - lines * 40.0).max(0.0);
}

/// Shows the browser while it is open and lists the catalog entries that
/// match the search whenever the search changes.
fn refresh_object_browser(
    mut commands: Commands,
    view: SelectionView,
    editor: Res<MapEditor>,
    mut browser: Single<&mut Visibility, With<ObjectBrowser>>,
    mut title: Single<&mut Text, With<BrowserTitle>>,
    list: Single<(Entity, &mut ScrollPosition), With<BrowserList>>,
    mut listed: Local<Option<String>>,
) {
    let wanted = if editor.browser_open && !menus_open(&view) {
        Visibility::Visible
    } else {
        Visibility::Hidden
    };
    if **browser != wanted {
        **browser = wanted;
    }
    if !editor.browser_open || listed.as_deref() == Some(editor.search.as_str()) {
        return;
    }
    *listed = Some(editor.search.clone());
    let matches = editor.matching_catalog();
    title.0 = format!(
        "Objects in this map: {} meshes, {} shown\nSearch: {}_\nHover the list and type to search (Backspace erases).\nClick an object to place it where the view is centred.",
        editor.catalog.len(),
        matches.len().min(BROWSER_LIMIT),
        editor.search,
    );
    let (list, mut scroll) = list.into_inner();
    scroll.y = 0.0;
    commands.entity(list).despawn_related::<Children>();
    commands.entity(list).with_children(|list| {
        for index in matches.into_iter().take(BROWSER_LIMIT) {
            let entry = &editor.catalog[index];
            list.spawn((
                Button,
                Node {
                    width: percent(100),
                    min_height: px(28),
                    padding: UiRect::axes(px(8), px(4)),
                    border_radius: BorderRadius::all(px(3)),
                    ..default()
                },
                BackgroundColor(BUTTON_COLOR),
                EditorAction::Place(index),
                BrowserItem,
            ))
            .with_children(|button| {
                button.spawn((
                    Text::new(format!("{}  ({}x)", entry.mesh, entry.uses)),
                    TextFont {
                        font_size: FontSize::Px(14.0),
                        ..default()
                    },
                    TextColor(Color::WHITE),
                ));
            });
        }
    });
}

fn show_editor_panel(
    view: SelectionView,
    editor: Res<MapEditor>,
    mut panel: Single<&mut Visibility, With<EditorPanel>>,
    mut text: Single<&mut Text, With<EditorPanelText>>,
    mut buttons: Query<(&Interaction, &EditorAction, &mut BackgroundColor)>,
) {
    let selection = editor
        .selected
        .and_then(|export| Some((editor.actors.get(&export)?, editor.placement(export)?)));
    let visible = selection.is_some() && !menus_open(&view);
    let wanted = if visible {
        Visibility::Visible
    } else {
        Visibility::Hidden
    };
    if **panel != wanted {
        **panel = wanted;
    }
    if editor.is_changed()
        && let Some((actor, placement)) = selection
    {
        text.0 = format!(
            "{}\n{}\nX {:.0}   Y {:.0}   Z {:.0}\nYaw {:.1} deg",
            actor.name,
            actor.mesh,
            placement.location.x,
            placement.location.y,
            placement.location.z,
            placement.rotation.y as f32 * 360.0 / ROTATOR_UNITS as f32,
        );
    }
    for (interaction, action, mut color) in &mut buttons {
        let active = *action == EditorAction::Step(editor.step);
        color.0 = match interaction {
            Interaction::Pressed => BUTTON_ACTIVE_COLOR,
            Interaction::Hovered => BUTTON_HOVER_COLOR,
            Interaction::None if active => BUTTON_ACTIVE_COLOR,
            Interaction::None => BUTTON_COLOR,
        };
    }
}

fn press_editor_buttons(
    mut commands: Commands,
    highlight: Res<SelectionState>,
    actions: Query<(&Interaction, &EditorAction), Changed<Interaction>>,
    camera: Single<&GlobalTransform, With<FlyCamera>>,
    scene: Res<MapScene>,
    mut editor: ResMut<MapEditor>,
    mut ray_cast: MeshRayCast,
    mut parts: Query<(Entity, &mut Transform, &mut SelectableObject)>,
    bounds: Query<(&Aabb, &GlobalTransform)>,
) {
    for (interaction, action) in &actions {
        if *interaction != Interaction::Pressed {
            continue;
        }
        if let EditorAction::Step(step) = *action {
            editor.step = step;
            continue;
        }
        match *action {
            EditorAction::Undo => undo(&mut commands, &mut editor, &mut parts, scene.origin),
            EditorAction::Save => save(&mut editor),
            EditorAction::Close => editor.selected = None,
            EditorAction::ToggleBrowser => editor.browser_open = !editor.browser_open,
            EditorAction::Place(index) => {
                let Some(entry) = editor.catalog.get(index) else {
                    continue;
                };
                let (template, mesh) = (entry.template, entry.mesh.clone());
                let placement = Placement {
                    location: placement_point(&mut ray_cast, &parts, &camera, scene.origin),
                    rotation: IVec3::ZERO,
                };
                let short = mesh.rsplit('.').next().unwrap_or(&mesh).to_string();
                let Some(export) = editor.add_placed(template, placement, format!("New {short}"))
                else {
                    continue;
                };
                spawn_copy(
                    &mut commands,
                    &mut editor,
                    &parts,
                    &highlight,
                    scene.origin,
                    template,
                    export,
                );
                editor.status = format!("Placed {mesh}; it is selected (saved with the map)");
            }
            EditorAction::Copy => {
                // Beside the original, a few steps to the camera's right.
                let offset =
                    nudge_direction(Nudge::Right, &camera) * (editor.step * 4.0).max(64.0);
                copy_selection(
                    &mut commands,
                    &mut editor,
                    &parts,
                    &highlight,
                    scene.origin,
                    offset,
                );
            }
            _ => {}
        }
        let Some(export) = editor.selected else {
            continue;
        };
        let Some(mut placement) = editor.placement(export) else {
            continue;
        };
        match *action {
            EditorAction::Move(nudge) => {
                placement.location += nudge_direction(nudge, &camera) * editor.step;
            }
            EditorAction::Rotate(yaw) => placement = rotated(placement, yaw),
            EditorAction::DropToFloor => {
                match drop_distance(&mut ray_cast, &parts, &bounds, export) {
                    Some(distance) => {
                        placement.location.z = (placement.location.z - distance).round();
                        editor.status = format!("Dropped {distance:.0} units to the floor");
                    }
                    None => editor.status = "No floor found below the selection".into(),
                }
            }
            _ => continue,
        }
        change_placement(&mut editor, &mut parts, scene.origin, export, placement);
    }
}

fn draw_selection_axes(scene: Res<MapScene>, editor: Res<MapEditor>, mut gizmos: Gizmos) {
    let Some(placement) = editor.selected.and_then(|export| editor.placement(export)) else {
        return;
    };
    let origin = viewer_translation(placement.location, scene.origin);
    let length = 3.0;
    // Unreal X, Y and Z in red, green and blue.
    gizmos.arrow(origin, origin + Vec3::X * length, Color::srgb(1.0, 0.2, 0.2));
    gizmos.arrow(origin, origin + Vec3::Z * length, Color::srgb(0.2, 1.0, 0.2));
    gizmos.arrow(origin, origin + Vec3::Y * length, Color::srgb(0.3, 0.5, 1.0));
}

#[derive(Component)]
struct EditorReadout;

fn spawn_editor_readout(mut commands: Commands) {
    commands.spawn((
        Name::new("Map editor"),
        Text::new(""),
        TextFont {
            font_size: FontSize::Px(15.0),
            ..default()
        },
        TextColor(Color::WHITE),
        Node {
            position_type: PositionType::Absolute,
            left: px(12),
            bottom: px(12),
            max_width: px(760),
            padding: UiRect::axes(px(10), px(7)),
            border_radius: BorderRadius::all(px(4)),
            ..default()
        },
        BackgroundColor(Color::srgba(0.02, 0.025, 0.035, 0.86)),
        EditorReadout,
    ));
}

fn update_editor_readout(
    editor: Res<MapEditor>,
    mut readout: Single<&mut Text, With<EditorReadout>>,
) {
    if !editor.is_changed() {
        return;
    }
    readout.0 = editor_summary(&editor);
}

fn editor_summary(editor: &MapEditor) -> String {
    let changes = match editor.unsaved_changes() {
        0 => "no unsaved changes".to_string(),
        1 => "1 unsaved change".to_string(),
        count => format!("{count} unsaved changes"),
    };
    let mut summary = format!(
        "EDITOR - {changes}\nClick a placed mesh to edit it; drag it to move, mouse wheel or Alt+drag for height. Ctrl+Z undo, Ctrl+S save."
    );
    if !editor.status.is_empty() {
        summary.push('\n');
        summary.push_str(&editor.status);
    }
    summary
}

#[cfg(test)]
mod tests {
    use super::*;

    fn editor() -> MapEditor {
        let instance = MeshInstance {
            name: "StaticMeshActor1".into(),
            mesh: "package.mesh".into(),
            location: Vec3::ZERO,
            rotation: Vec3::ZERO,
            scale: Vec3::ONE,
            material_override: MaterialOverride::default(),
            source: Some(ActorSource {
                export: 7,
                location: Vec3::new(100.0, 200.0, 300.0),
                rotation: IVec3::new(0, 16384, 0),
                col_location: Some(Vec3::new(100.0, 200.0, 340.0)),
                static_mesh_actor: true,
            }),
        };
        MapEditor::new(Path::new("map.unr"), Vec::new(), &[instance])
    }

    #[test]
    fn moving_an_actor_edits_location_and_follows_with_collision_location() {
        let mut editor = editor();
        editor.set_placement(
            7,
            Placement {
                location: Vec3::new(150.0, 200.0, 280.5),
                rotation: IVec3::new(0, 16384, 0),
            },
        );
        let edits = editor.property_edits();
        assert_eq!(edits.len(), 2);
        assert_eq!(
            (edits[0].name.as_str(), edits[0].value.as_str()),
            ("Location", "150,200,280.5")
        );
        assert_eq!(
            (edits[1].name.as_str(), edits[1].value.as_str()),
            ("ColLocation", "150,200,320.5")
        );
        assert_eq!(editor.unsaved_changes(), 1);
    }

    #[test]
    fn rotating_an_actor_edits_only_its_rotator() {
        let mut editor = editor();
        editor.set_placement(
            7,
            Placement {
                location: Vec3::new(100.0, 200.0, 300.0),
                rotation: IVec3::new(0, 24576, 0),
            },
        );
        let edits = editor.property_edits();
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].property_type, "Rotator");
        assert_eq!(edits[0].value, "0,24576,0");
    }

    #[test]
    fn returning_an_actor_to_its_original_placement_clears_the_edit() {
        let mut editor = editor();
        let original = editor.placement(7).unwrap();
        editor.set_placement(
            7,
            Placement {
                location: Vec3::ZERO,
                ..original
            },
        );
        editor.set_placement(7, original);
        assert!(editor.property_edits().is_empty());
        assert_eq!(editor.unsaved_changes(), 0);
    }

    #[test]
    #[ignore = "requires LINEAGE2_CLIENT to point to a complete client"]
    fn saving_moves_an_actor_in_an_encrypted_lineage_map() {
        let client = PathBuf::from(
            std::env::var_os("LINEAGE2_CLIENT").expect("LINEAGE2_CLIENT is not configured"),
        );
        let directory = std::env::temp_dir().join(format!(
            "unreal-viewer-editor-save-{}",
            std::process::id()
        ));
        fs::create_dir_all(&directory).unwrap();
        let map = directory.join("24_18.unr");
        fs::copy(client.join("Maps/24_18.unr"), &map).unwrap();
        let original_bytes = fs::read(&map).unwrap();

        let (scene, _) = load_scene(&map, &[]).unwrap();
        let mut editor = MapEditor::new(&map, original_bytes.clone(), &scene.instances);
        let gate = scene
            .instances
            .iter()
            .find(|instance| instance.name == "StaticMeshActor1")
            .and_then(|instance| instance.source)
            .unwrap();
        let moved = Placement {
            location: gate.location + Vec3::new(0.0, 0.0, 2000.0),
            rotation: IVec3::new(0, 16384, 0),
        };
        editor.set_placement(gate.export, moved);
        let backup = editor.save().unwrap();

        assert_eq!(fs::read(&backup).unwrap(), original_bytes);
        assert_eq!(editor.unsaved_changes(), 0);
        let (saved, _) = load_scene(&map, &[]).unwrap();
        let saved_gate = saved
            .instances
            .iter()
            .find_map(|instance| instance.source.filter(|source| source.export == gate.export))
            .unwrap();
        assert_eq!(saved_gate.location, moved.location);
        assert_eq!(saved_gate.rotation, moved.rotation);
        assert_eq!(
            saved_gate.col_location,
            gate.col_location.map(|value| value + Vec3::new(0.0, 0.0, 2000.0))
        );
        assert_eq!(saved.instances.len(), scene.instances.len());

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn camera_relative_moves_snap_to_the_nearest_map_axis() {
        // Viewer -Z is Unreal -Y; a slightly turned camera still moves along it.
        assert_eq!(
            snapped_horizontal_axis(Vec3::new(0.3, -0.5, -1.0)),
            Vec3::new(0.0, -1.0, 0.0)
        );
        assert_eq!(
            snapped_horizontal_axis(Vec3::new(1.0, 0.0, 0.2)),
            Vec3::new(1.0, 0.0, 0.0)
        );
    }

    #[test]
    fn rotating_wraps_yaw_into_one_turn() {
        let placement = Placement {
            location: Vec3::ZERO,
            rotation: IVec3::new(0, 1000, 0),
        };
        assert_eq!(rotated(placement, -16384).rotation.y, 65536 - 15384);
    }

    #[test]
    #[ignore = "requires LINEAGE2_CLIENT to point to a complete client"]
    fn adding_an_actor_to_an_encrypted_lineage_map_keeps_the_ambient_sound_list() {
        use unreal_package::unreal::ActorAddition;
        let client = PathBuf::from(
            std::env::var_os("LINEAGE2_CLIENT").expect("LINEAGE2_CLIENT is not configured"),
        );
        let wire = fs::read(client.join("Maps/24_18.unr")).unwrap();
        let package = Package::from_wire_bytes(Path::new("24_18.unr"), wire.clone()).unwrap();
        let level_actors = |package: &Package| {
            package
                .dump()
                .exports
                .into_iter()
                .find_map(|export| match export.native_object {
                    Some(NativeObject::Level(level)) => Some(level.actors.len()),
                    _ => None,
                })
                .unwrap()
        };
        let before = level_actors(&package);
        let gate = package
            .dump()
            .exports
            .iter()
            .position(|export| export.name == "StaticMeshActor1")
            .unwrap();
        let (plain, index) = package
            .add_actor(&ActorAddition {
                template: gate,
                static_mesh: None,
                location: "147456,5900,5776".into(),
                rotation: "0,32768,0".into(),
            })
            .unwrap();
        let saved = reencrypt_like(&wire, plain, "24_18.unr").unwrap();

        let directory = std::env::temp_dir().join(format!(
            "unreal-viewer-editor-add-{}",
            std::process::id()
        ));
        fs::create_dir_all(&directory).unwrap();
        let map = directory.join("24_18.unr");
        fs::write(&map, &saved).unwrap();
        let reread = Package::read(&map).unwrap();
        // That list is the level's ambient sounds; the client crashes if a
        // mesh actor is added to it.
        assert_eq!(level_actors(&reread), before);
        assert_eq!(reread.export_count(), package.export_count() + 1);
        let (scene, _) = load_scene(&map, &[]).unwrap();
        let copy = scene
            .instances
            .iter()
            .find_map(|instance| instance.source.filter(|source| source.export == index))
            .unwrap();
        assert_eq!(copy.location, Vec3::new(147456.0, 5900.0, 5776.0));
        assert_eq!(copy.rotation, IVec3::new(0, 32768, 0));
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn copies_are_numbered_after_the_package_and_undo_removes_them() {
        let mut editor = editor();
        editor.base_exports = 20;
        let placement = Placement {
            location: Vec3::new(164.0, 200.0, 300.0),
            rotation: IVec3::new(0, 16384, 0),
        };
        assert_eq!(editor.add_copy(7, placement), Some(20));
        assert_eq!(editor.placement(20), Some(placement));
        assert_eq!(editor.unsaved_changes(), 1);
        // Moving the copy changes the actor it saves, not a property edit.
        editor.set_placement(
            20,
            Placement {
                location: Vec3::ZERO,
                ..placement
            },
        );
        assert!(editor.property_edits().is_empty());

        assert_eq!(editor.undo.last(), Some(&UndoStep::Added(20)));
        editor.remove_addition(20);
        assert!(editor.additions.is_empty());
        assert_eq!(editor.placement(20), None);
        assert_eq!(editor.unsaved_changes(), 0);
    }

    #[test]
    #[ignore = "requires LINEAGE2_CLIENT to point to a complete client"]
    fn saving_a_copy_adds_the_actor_to_an_encrypted_lineage_map() {
        let client = PathBuf::from(
            std::env::var_os("LINEAGE2_CLIENT").expect("LINEAGE2_CLIENT is not configured"),
        );
        let directory = std::env::temp_dir().join(format!(
            "unreal-viewer-editor-copy-{}",
            std::process::id()
        ));
        fs::create_dir_all(&directory).unwrap();
        let map = directory.join("24_18.unr");
        fs::copy(client.join("Maps/24_18.unr"), &map).unwrap();
        let (scene, _) = load_scene(&map, &[]).unwrap();
        let mut editor = MapEditor::new(&map, fs::read(&map).unwrap(), &scene.instances);
        let gate = scene
            .instances
            .iter()
            .find(|instance| instance.name == "StaticMeshActor1")
            .and_then(|instance| instance.source)
            .unwrap();
        let placement = Placement {
            location: gate.location + Vec3::new(0.0, 512.0, 0.0),
            rotation: gate.rotation,
        };
        let copy = editor.add_copy(gate.export, placement).unwrap();
        // Copy a copy, then move the first one: both save in order.
        let second = editor
            .add_copy(copy, Placement {
                location: placement.location + Vec3::new(0.0, 512.0, 0.0),
                ..placement
            })
            .unwrap();
        let moved = Placement {
            location: placement.location + Vec3::new(64.0, 0.0, 0.0),
            ..placement
        };
        editor.set_placement(copy, moved);
        editor.save().unwrap();
        assert_eq!(editor.unsaved_changes(), 0);

        let (saved, _) = load_scene(&map, &[]).unwrap();
        assert_eq!(saved.instances.len(), scene.instances.len() + 2);
        let location = |export| {
            saved
                .instances
                .iter()
                .find_map(|instance| instance.source.filter(|source| source.export == export))
                .map(|source| source.location)
        };
        assert_eq!(location(copy), Some(moved.location));
        assert_eq!(
            location(second),
            Some(placement.location + Vec3::new(0.0, 512.0, 0.0))
        );
        assert_eq!(location(gate.export), Some(gate.location));
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn the_catalog_lists_each_static_mesh_once_and_searches_by_words() {
        let instance = |name: &str, mesh: &str, export, static_mesh_actor| MeshInstance {
            name: name.into(),
            mesh: mesh.into(),
            location: Vec3::ZERO,
            rotation: Vec3::ZERO,
            scale: Vec3::ONE,
            material_override: MaterialOverride::default(),
            source: Some(ActorSource {
                export,
                location: Vec3::ZERO,
                rotation: IVec3::ZERO,
                col_location: None,
                static_mesh_actor,
            }),
        };
        let mut editor = MapEditor::new(
            Path::new("map.unr"),
            Vec::new(),
            &[
                instance("A", "Aden_Village_S.Aden_Platform01", 3, true),
                instance("B", "aden_village_s.aden_platform01", 4, true),
                instance("C", "Aden_Castle_S.Aden_castle_Maingate", 5, true),
                instance("D", "Aden_Castle_S.Door_Mover", 6, false),
            ],
        );
        assert_eq!(editor.catalog.len(), 2);
        assert_eq!(editor.catalog[1].mesh, "Aden_Village_S.Aden_Platform01");
        assert_eq!(editor.catalog[1].template, 3);
        assert_eq!(editor.catalog[1].uses, 2);

        editor.search = "aden  PLATFORM".into();
        assert_eq!(editor.matching_catalog(), [1]);
        editor.search = "castle gate".into();
        assert_eq!(editor.matching_catalog(), [0]);
        editor.search.clear();
        assert_eq!(editor.matching_catalog(), [0, 1]);
    }

    #[test]
    fn viewer_drag_deltas_map_back_to_unreal_axes() {
        assert_eq!(
            unreal_delta(Vec3::new(1.0, 2.0, -3.0)),
            Vec3::new(128.0, -384.0, 256.0)
        );
    }
}


