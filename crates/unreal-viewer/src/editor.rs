//! In-viewer map editing: select a placed mesh actor, move, turn or drop it
//! with the editor panel or by dragging, undo, and save the edited placements
//! back into the map package.

use bevy::camera::primitives::Aabb;
use unreal_package::unreal::{PropertyEdit, reencrypt_like};

use super::*;

/// Rotator units in one full turn.
const ROTATOR_UNITS: i32 = 65_536;
/// Move steps offered by the panel, in Unreal units.
const STEP_SIZES: [f32; 4] = [1.0, 16.0, 64.0, 256.0];
/// How far the cursor moves before a press on the selection becomes a drag.
const DRAG_THRESHOLD: f32 = 4.0;

pub(super) struct MapEditorPlugin;

impl Plugin for MapEditorPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, (spawn_editor_readout, spawn_editor_panel))
            .add_systems(
                Update,
                (
                    track_panel_pointer
                        .before(drag_selected_actor)
                        .before(runtime::select_rendered_object),
                    drag_selected_actor.before(runtime::select_rendered_object),
                    finish_drag.after(runtime::select_rendered_object),
                    press_editor_buttons,
                    editor_shortcuts,
                    show_editor_panel.after(press_editor_buttons),
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
    /// Move step of the panel buttons, in Unreal units.
    step: f32,
    pointer_over_panel: bool,
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
        Self {
            path: path.to_path_buf(),
            wire,
            actors,
            edits: BTreeMap::new(),
            saved: BTreeMap::new(),
            undo: Vec::new(),
            selected: None,
            drag: None,
            step: STEP_SIZES[1],
            pointer_over_panel: false,
            status: String::new(),
        }
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

/// Keyboard chords kept alongside the panel buttons.
fn editor_shortcuts(
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
        undo(&mut editor, &mut parts, scene.origin);
    }
    if keys.just_pressed(KeyCode::KeyS) {
        save(&mut editor);
    }
}

fn undo(
    editor: &mut MapEditor,
    parts: &mut Query<(Entity, &mut Transform, &mut SelectableObject)>,
    scene_origin: Vec3,
) {
    match editor.undo.pop() {
        Some((export, placement)) => {
            editor.set_placement(export, placement);
            apply_placement(parts, scene_origin, export, placement);
            editor.selected = Some(export);
            editor.status = "Undone".into();
        }
        None => editor.status = "Nothing to undo".into(),
    }
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
    editor.undo.push((export, before));
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
            [("Drop to floor".to_string(), EditorAction::DropToFloor)],
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
    panel: Single<(&Interaction, &Visibility), With<EditorPanel>>,
    mut editor: ResMut<MapEditor>,
) {
    let (interaction, visibility) = *panel;
    let over = *visibility != Visibility::Hidden && *interaction != Interaction::None;
    if editor.pointer_over_panel != over {
        editor.pointer_over_panel = over;
    }
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
            EditorAction::Undo => undo(&mut editor, &mut parts, scene.origin),
            EditorAction::Save => save(&mut editor),
            EditorAction::Close => editor.selected = None,
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
        "EDITOR - {changes}\nClick a placed mesh to edit it; drag it to move, Alt+drag for height. Ctrl+Z undo, Ctrl+S save."
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
    fn viewer_drag_deltas_map_back_to_unreal_axes() {
        assert_eq!(
            unreal_delta(Vec3::new(1.0, 2.0, -3.0)),
            Vec3::new(128.0, -384.0, 256.0)
        );
    }
}
