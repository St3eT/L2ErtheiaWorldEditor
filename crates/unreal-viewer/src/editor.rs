//! In-viewer map editing: drag and rotate placed mesh actors, undo, and save
//! the edited placements back into the map package.

use unreal_package::unreal::{PropertyEdit, reencrypt_like};

use super::*;

/// Rotator units in one full turn.
const ROTATOR_UNITS: i32 = 65_536;
const ROTATION_STEP: i32 = ROTATOR_UNITS / 8;
const FINE_ROTATION_STEP: i32 = ROTATOR_UNITS / 64;
const HEIGHT_STEP: f32 = 16.0;
const FINE_HEIGHT_STEP: f32 = 1.0;
/// How far the cursor moves before a press on the selection becomes a drag.
const DRAG_THRESHOLD: f32 = 4.0;

pub(super) struct MapEditorPlugin;

impl Plugin for MapEditorPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, spawn_editor_readout).add_systems(
            Update,
            (
                drag_selected_actor.before(runtime::select_rendered_object),
                finish_drag.after(runtime::select_rendered_object),
                edit_selected_actor,
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
    original: Placement,
    col_location: Option<Vec3>,
}

struct Drag {
    export: usize,
    start_cursor: Vec2,
    plane_origin: Vec3,
    plane_normal: Vec3,
    start_hit: Vec3,
    vertical: bool,
    start: Placement,
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
    undo: Vec<(usize, Placement)>,
    pub(super) selected: Option<usize>,
    drag: Option<Drag>,
    status: String,
}

impl MapEditor {
    pub(super) fn new(path: &Path, wire: Vec<u8>, instances: &[MeshInstance]) -> Self {
        let actors = instances
            .iter()
            .filter_map(|instance| {
                let source = instance.source?;
                Some((
                    source.export,
                    EditableActor {
                        name: instance.name.clone(),
                        original: Placement {
                            location: source.location,
                            rotation: source.rotation,
                        },
                        col_location: source.col_location,
                    },
                ))
            })
            .collect();
        Self {
            path: path.to_path_buf(),
            wire,
            actors,
            edits: BTreeMap::new(),
            saved: BTreeMap::new(),
            undo: Vec::new(),
            selected: None,
            drag: None,
            status: String::new(),
        }
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
        exports
            .into_iter()
            .filter(|export| self.edits.get(export) != self.saved.get(export))
            .count()
    }

    fn property_edits(&self) -> Vec<PropertyEdit> {
        let mut edits = Vec::new();
        for (&export, placement) in &self.edits {
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
        let package = Package::from_wire_bytes(&self.path, self.wire.clone())?;
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
    Vec3::new(delta.x, -delta.z, delta.y) * UNREAL_UNITS_PER_METER
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
        editor.undo.push((drag.export, drag.start));
    }
}

fn edit_selected_actor(
    view: SelectionView,
    scene: Res<MapScene>,
    mut editor: ResMut<MapEditor>,
    mut parts: Query<(Entity, &mut Transform, &mut SelectableObject)>,
) {
    if menus_open(&view) || editor.is_dragging() {
        return;
    }
    let keys = &view.keys;
    let control = keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::ControlRight);
    let fine = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
    if control && keys.just_pressed(KeyCode::KeyZ) {
        match editor.undo.pop() {
            Some((export, placement)) => {
                editor.set_placement(export, placement);
                apply_placement(&mut parts, scene.origin, export, placement);
                editor.selected = Some(export);
                editor.status = "Undone".into();
            }
            None => editor.status = "Nothing to undo".into(),
        }
        return;
    }
    if control && keys.just_pressed(KeyCode::KeyS) {
        editor.status = match editor.save() {
            Ok(backup) => format!(
                "Saved {} (original kept as {})",
                editor.path.display(),
                backup.display()
            ),
            Err(error) => format!("Save failed: {error}"),
        };
        eprintln!("viewer: {}", editor.status);
        return;
    }
    if control {
        return;
    }
    let Some(export) = editor.selected else {
        return;
    };
    let Some(before) = editor.placement(export) else {
        return;
    };
    let mut placement = before;
    let rotation_step = if fine {
        FINE_ROTATION_STEP
    } else {
        ROTATION_STEP
    };
    let height_step = if fine { FINE_HEIGHT_STEP } else { HEIGHT_STEP };
    if keys.just_pressed(KeyCode::KeyR) {
        placement.rotation.y = (placement.rotation.y + rotation_step).rem_euclid(ROTATOR_UNITS);
    }
    if keys.just_pressed(KeyCode::KeyF) {
        placement.rotation.y = (placement.rotation.y - rotation_step).rem_euclid(ROTATOR_UNITS);
    }
    if keys.just_pressed(KeyCode::PageUp) {
        placement.location.z += height_step;
    }
    if keys.just_pressed(KeyCode::PageDown) {
        placement.location.z -= height_step;
    }
    if placement != before {
        editor.undo.push((export, before));
        editor.set_placement(export, placement);
        apply_placement(&mut parts, scene.origin, export, placement);
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
    gizmos.arrow(origin, origin - Vec3::Z * length, Color::srgb(0.2, 1.0, 0.2));
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
    let selection = match editor
        .selected
        .and_then(|export| Some((editor.actors.get(&export)?, editor.placement(export)?)))
    {
        Some((actor, placement)) => format!(
            "Selected {}: X {:.0}  Y {:.0}  Z {:.0}  Yaw {:.1}°",
            actor.name,
            placement.location.x,
            placement.location.y,
            placement.location.z,
            placement.rotation.y as f32 * 360.0 / ROTATOR_UNITS as f32,
        ),
        None => "Click a placed mesh to select it for editing".into(),
    };
    let mut summary = format!(
        "EDITOR — {changes}\n{selection}\nDrag: move  ·  Alt+drag: height  ·  R/F: rotate  ·  PgUp/PgDn: height  ·  Shift: fine  ·  Ctrl+Z: undo  ·  Ctrl+S: save"
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
    fn viewer_drag_deltas_map_back_to_unreal_axes() {
        assert_eq!(
            unreal_delta(Vec3::new(1.0, 2.0, -3.0)),
            Vec3::new(128.0, 384.0, 256.0)
        );
    }
}
