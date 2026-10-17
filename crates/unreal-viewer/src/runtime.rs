use super::*;
use crate::presentation::{game_event_row_label, game_event_summary, time_of_day_summary};

pub(super) struct ViewerRuntimePlugin;

impl Plugin for ViewerRuntimePlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Update,
            (
                fly_camera,
                select_rendered_object,
                update_coordinate_readout,
                particles::simulate_particles,
                pan_material_textures,
                flip_texture_frames,
                follow_sky_camera,
                (advance_time_of_day, apply_time_of_day).chain(),
                animate_skeletons,
                adjust_lighting,
                toggle_teleport_panel,
                teleport_from_panel,
                scroll_teleport_panel,
                toggle_world_map,
                interact_with_world_map,
                toggle_game_event_panel,
                interact_with_game_events,
                toggle_settings_panel,
                interact_with_settings,
            ),
        );
    }
}

fn advance_time_of_day(time: Res<Time>, mut clock: ResMut<TimeOfDay>) {
    if clock.running {
        clock.advance(time.delta_secs());
    }
}

// Lineage swings the sun across its sky room on its own clock, keeps the moon
// on the far side, and turns the sunlight to match.
fn apply_time_of_day(
    clock: Res<TimeOfDay>,
    path: Res<SunPath>,
    room: Option<Res<SkyRoom>>,
    mut control: ResMut<LightingControl>,
    mut ambient: ResMut<GlobalAmbientLight>,
    mut bodies: Query<(&mut Transform, &mut Visibility, &SkyBodyPhase)>,
    mut sunlight: Query<(&mut DirectionalLight, &mut Transform, &Daylight), Without<SkyBodyPhase>>,
) {
    let sun = clock.sun_direction(&path);
    control.daylight = TimeOfDay::daylight(sun.y);
    ambient.brightness = control.brightness();
    for (mut transform, mut visibility, phase) in &mut bodies {
        let direction = if phase.night { -sun } else { sun };
        if let Some(room) = &room {
            transform.translation =
                direction * crate::presentation::sky_wall_distance(room.bounds, direction) * 0.85
                    / UNREAL_UNITS_PER_METER;
            transform.look_at(Vec3::ZERO, Vec3::Y);
        }
        *visibility = if direction.y > -0.05 {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
    }
    for (mut light, mut transform, daylight) in &mut sunlight {
        light.illuminance = daylight.illuminance * control.daylight;
        transform.look_to(-sun, Vec3::Y);
    }
}

// An Unreal sky only ever turns: it is drawn from its own zone actor, so the
// player never moves relative to it however far they walk.
fn follow_sky_camera(
    camera: Query<&Transform, (With<FlyCamera>, Without<SkyCamera>)>,
    mut sky: Query<&mut Transform, (With<SkyCamera>, Without<FlyCamera>)>,
) {
    let Ok(view) = camera.single() else {
        return;
    };
    for mut transform in &mut sky {
        transform.rotation = view.rotation;
    }
}

// Unreal scrolls a panner's texture coordinates rather than its geometry, so
// water sheets and waterfalls flow without moving.
fn pan_material_textures(
    time: Res<Time>,
    panning: Res<PanningMaterials>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let seconds = time.elapsed_secs();
    for (handle, motion) in &panning.0 {
        let Some(mut material) = materials.get_mut(handle) else {
            continue;
        };
        material.uv_transform.translation = motion.offset(seconds);
    }
}

// Unreal plays an animated texture by chaining its frames; the viewer swaps
// which frame a material draws at the texture's own rate.
// Lineage's water flipbooks are authored at 15 fps, which steps visibly on a
// modern display; playing them faster keeps the flow from looking sluggish.
const FLIPBOOK_SPEEDUP: f32 = 2.0;

fn flip_texture_frames(
    time: Res<Time>,
    animated: Res<AnimatedMaterials>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let seconds = time.elapsed_secs();
    for (handle, frames, rate) in &animated.0 {
        let frame = &frames[(seconds * rate * FLIPBOOK_SPEEDUP) as usize % frames.len()];
        if materials
            .get(handle)
            .is_some_and(|material| material.base_color_texture.as_ref() == Some(frame))
        {
            continue;
        }
        if let Some(mut material) = materials.get_mut(handle) {
            material.base_color_texture = Some(frame.clone());
        }
    }
}

fn animate_skeletons(
    time: Res<Time>,
    skeletons: Query<&AnimatedSkeleton>,
    mut transforms: Query<&mut Transform>,
) {
    for skeleton in &skeletons {
        if skeleton.animation.duration <= 0.0 {
            continue;
        }
        let playback_time = time.elapsed_secs() % skeleton.animation.duration;
        for (joint, track) in skeleton.joints.iter().zip(&skeleton.animation.tracks) {
            let Some(track) = track else {
                continue;
            };
            let Ok(mut transform) = transforms.get_mut(*joint) else {
                continue;
            };
            if !track.rotations.is_empty() {
                transform.rotation = sample_rotation(track, playback_time);
            }
            if !track.translations.is_empty() {
                transform.translation = sample_translation(track, playback_time);
            }
        }
    }
}

fn adjust_lighting(
    keys: Res<ButtonInput<KeyCode>>,
    settings: Res<SettingsMenuState>,
    mut ambient: ResMut<GlobalAmbientLight>,
    mut control: ResMut<LightingControl>,
) {
    if settings.open {
        return;
    }
    let increase = keys.just_pressed(KeyCode::Equal) || keys.just_pressed(KeyCode::NumpadAdd);
    let decrease = keys.just_pressed(KeyCode::Minus) || keys.just_pressed(KeyCode::NumpadSubtract);
    let direction = i8::from(increase) - i8::from(decrease);
    if direction == 0 {
        return;
    }
    control.multiplier = lighting_multiplier(control.multiplier, direction);
    ambient.brightness = control.brightness();
    eprintln!(
        "viewer: ambient lighting {:.0}% ({:.1} brightness)",
        control.multiplier * 100.0,
        ambient.brightness
    );
}

fn toggle_teleport_panel(
    keys: Res<ButtonInput<KeyCode>>,
    settings: Res<SettingsMenuState>,
    mut state: ResMut<TeleportMenuState>,
    mut world_map: ResMut<WorldMapState>,
    mut game_events: ResMut<GameEventMenuState>,
    panels: Query<(&mut Visibility, &OverlayPanelKind)>,
) {
    if settings.open || !keys.just_pressed(KeyCode::KeyT) {
        return;
    }
    state.open = !state.open;
    if state.open {
        world_map.open = false;
        game_events.open = false;
    }
    show_only_panel(panels, OverlayPanelKind::Teleport, state.open);
}

// One overlay at a time: opening a panel hides whichever was up.
fn show_only_panel(
    mut panels: Query<(&mut Visibility, &OverlayPanelKind)>,
    wanted: OverlayPanelKind,
    open: bool,
) {
    for (mut visibility, panel) in &mut panels {
        if *panel == wanted {
            *visibility = if open {
                Visibility::Visible
            } else {
                Visibility::Hidden
            };
        } else if open {
            *visibility = Visibility::Hidden;
        }
    }
}

fn teleport_from_panel(
    scene: Res<MapScene>,
    mut state: ResMut<TeleportMenuState>,
    mut panel: Single<&mut Visibility, With<TeleportPanel>>,
    buttons: Query<(&Interaction, &TeleportButton), Changed<Interaction>>,
    mut camera: Single<&mut Transform, With<FlyCamera>>,
) {
    if !state.open {
        return;
    }
    let Some(target) = buttons.iter().find_map(|(interaction, button)| {
        (*interaction == Interaction::Pressed).then_some(button.location)
    }) else {
        return;
    };
    camera.translation = (target - scene.origin) / UNREAL_UNITS_PER_METER;
    state.open = false;
    **panel = Visibility::Hidden;
}

fn scroll_teleport_panel(
    state: Res<TeleportMenuState>,
    scroll: Res<AccumulatedMouseScroll>,
    mut list: Single<&mut ScrollPosition, With<TeleportScroll>>,
) {
    if !state.open || scroll.delta.y == 0.0 {
        return;
    }
    list.y = (list.y - scroll.delta.y * 36.0).max(0.0);
}

fn toggle_world_map(
    keys: Res<ButtonInput<KeyCode>>,
    settings: Res<SettingsMenuState>,
    mut state: ResMut<WorldMapState>,
    mut teleport: ResMut<TeleportMenuState>,
    mut game_events: ResMut<GameEventMenuState>,
    panels: Query<(&mut Visibility, &OverlayPanelKind)>,
) {
    if settings.open || !keys.just_pressed(KeyCode::KeyM) {
        return;
    }
    state.open = !state.open;
    if state.open {
        teleport.open = false;
        game_events.open = false;
    }
    show_only_panel(panels, OverlayPanelKind::WorldMap, state.open);
}

fn interact_with_world_map(
    state: Res<WorldMapState>,
    image: Option<Single<(&Interaction, &RelativeCursorPosition), With<WorldMapImage>>>,
    navigation: Res<MapNavigation>,
    mut hover_text: Single<&mut Text, With<WorldMapHoverText>>,
    mut exit: MessageWriter<AppExit>,
) {
    if !state.open {
        return;
    }
    let Some(image) = image else {
        return;
    };
    let (interaction, cursor) = *image;
    let Some(normalized) = cursor.normalized.filter(|_| cursor.cursor_over()) else {
        hover_text.0 = "Move over the map to select a region".into();
        return;
    };
    let map_name = radar_map_name(normalized);
    let path = navigation.maps_dir.join(&map_name);
    hover_text.0 = if path.is_file() {
        format!("{map_name} - click to load")
    } else {
        format!("{map_name} - unavailable")
    };
    if *interaction == Interaction::Pressed && path.is_file() {
        eprintln!("viewer: loading {}", path.display());
        if let Err(error) = store_next_map(&navigation.next, path) {
            eprintln!("viewer: cannot hand selected map to the next viewer: {error}");
            return;
        }
        exit.write(AppExit::Success);
    }
}

fn toggle_game_event_panel(
    keys: Res<ButtonInput<KeyCode>>,
    settings: Res<SettingsMenuState>,
    mut state: ResMut<GameEventMenuState>,
    mut teleport: ResMut<TeleportMenuState>,
    mut world_map: ResMut<WorldMapState>,
    panels: Query<(&mut Visibility, &OverlayPanelKind)>,
) {
    if settings.open || !keys.just_pressed(KeyCode::KeyG) {
        return;
    }
    state.open = !state.open;
    if state.open {
        teleport.open = false;
        world_map.open = false;
    }
    show_only_panel(panels, OverlayPanelKind::GameEvents, state.open);
}

fn interact_with_game_events(
    state: Res<GameEventMenuState>,
    scroll: Res<AccumulatedMouseScroll>,
    mut events: ResMut<GameEventEffects>,
    rows: Query<(&Interaction, &GameEventRow), Changed<Interaction>>,
    mut labels: Query<(&mut Text, &GameEventRowText)>,
    mut list: Single<&mut ScrollPosition, With<GameEventScroll>>,
) {
    if !state.open {
        return;
    }
    if scroll.delta.y != 0.0 {
        list.y = (list.y - scroll.delta.y * 36.0).max(0.0);
    }
    for (interaction, row) in &rows {
        if *interaction == Interaction::Pressed {
            events.toggle(row.0);
        }
    }
    for (mut label, row) in &mut labels {
        label.0 = game_event_row_label(
            row.0,
            events.counts.get(&row.0).copied().unwrap_or_default(),
            events.playing.contains(&row.0),
        );
    }
}

fn toggle_settings_panel(
    keys: Res<ButtonInput<KeyCode>>,
    mut settings: ResMut<SettingsMenuState>,
    mut teleport: ResMut<TeleportMenuState>,
    mut world_map: ResMut<WorldMapState>,
    mut game_events: ResMut<GameEventMenuState>,
    panels: Query<(&mut Visibility, &OverlayPanelKind)>,
) {
    if !keys.just_pressed(KeyCode::Escape) {
        return;
    }
    settings.open = !settings.open;
    if settings.open {
        teleport.open = false;
        world_map.open = false;
        game_events.open = false;
    }
    show_only_panel(panels, OverlayPanelKind::Settings, settings.open);
}

fn interact_with_settings(
    mut commands: Commands,
    mut controls: SettingsControls,
    actions: Query<(&Interaction, &SettingsAction), Changed<Interaction>>,
    mut camera: Single<(Entity, &mut FlyCamera)>,
    mut labels: Query<(&mut Text, &SettingsLabel)>,
) {
    if !controls.menu.open {
        return;
    }
    let (state, events, time) = (
        &mut *controls.menu,
        &mut *controls.events,
        &mut *controls.time,
    );
    for (interaction, action) in &actions {
        if *interaction != Interaction::Pressed {
            continue;
        }
        match action {
            SettingsAction::ToggleFog => {
                let Some(fog) = state.fog.clone() else {
                    continue;
                };
                state.fog_enabled = !state.fog_enabled;
                if state.fog_enabled {
                    commands.entity(camera.0).insert(distance_fog(&fog));
                } else {
                    commands.entity(camera.0).remove::<DistanceFog>();
                }
            }
            SettingsAction::ToggleDayNight => time.skip_half_day(),
            SettingsAction::ShorterDay => time.change_day_length(false),
            SettingsAction::LongerDay => time.change_day_length(true),
            SettingsAction::ToggleGameEventEffects => events.toggle_all(),
            SettingsAction::Slower => camera.1.speed = adjusted_move_speed(camera.1.speed, -1),
            SettingsAction::Faster => camera.1.speed = adjusted_move_speed(camera.1.speed, 1),
            SettingsAction::ResetSpeed => camera.1.speed = state.initial_move_speed,
        }
    }
    for (mut label, kind) in &mut labels {
        label.0 = match kind {
            SettingsLabel::Fog => match (&state.fog, state.fog_enabled) {
                (None, _) => "Fog: unavailable on this map".into(),
                (Some(_), true) => "Fog: enabled (click to disable)".into(),
                (Some(_), false) => "Fog: disabled (click to enable)".into(),
            },
            SettingsLabel::DayNight => time_of_day_summary(time),
            SettingsLabel::GameEvents => game_event_summary(events),
            SettingsLabel::Speed => format!("Movement speed: {:.1} m/s", camera.1.speed),
        };
    }
}

fn fly_camera(input: FlyCameraInput, mut cameras: Query<(&mut Transform, &mut FlyCamera)>) {
    if input.teleport_menu.open
        || input.world_map.open
        || input.settings_menu.open
        || input.game_event_menu.open
    {
        return;
    }
    for (mut transform, mut camera) in &mut cameras {
        if input.mouse_buttons.pressed(MouseButton::Right) {
            camera.yaw -= input.mouse_motion.delta.x * 0.003;
            camera.pitch = (camera.pitch - input.mouse_motion.delta.y * 0.003).clamp(-1.54, 1.54);
        }
        transform.rotation = Quat::from_axis_angle(Vec3::Y, camera.yaw)
            * Quat::from_axis_angle(Vec3::X, camera.pitch);
        let mut movement = Vec3::ZERO;
        // Control chords such as Ctrl+S belong to the map editor.
        if input.keys.pressed(KeyCode::ControlLeft) || input.keys.pressed(KeyCode::ControlRight) {
            continue;
        }
        if input.keys.pressed(KeyCode::KeyW) {
            movement += *transform.forward();
        }
        if input.keys.pressed(KeyCode::KeyS) {
            movement += *transform.back();
        }
        if input.keys.pressed(KeyCode::KeyA) {
            movement += *transform.left();
        }
        if input.keys.pressed(KeyCode::KeyD) {
            movement += *transform.right();
        }
        if input.keys.pressed(KeyCode::KeyE) {
            movement += Vec3::Y;
        }
        if input.keys.pressed(KeyCode::KeyQ) {
            movement -= Vec3::Y;
        }
        let boost = if input.keys.pressed(KeyCode::ShiftLeft) {
            4.0
        } else {
            1.0
        };
        let speed = camera.speed;
        transform.translation +=
            movement.normalize_or_zero() * speed * boost * input.time.delta_secs();
    }
}

fn update_coordinate_readout(
    scene: Res<MapScene>,
    camera: Single<&Transform, With<FlyCamera>>,
    mut readout: Single<&mut Text, With<CoordinateReadout>>,
) {
    let unreal = camera_unreal_coordinates(camera.translation, scene.origin);
    readout.0 = format!("X {:.0}  Y {:.0}  Z {:.0}", unreal.x, unreal.y, unreal.z);
}

pub(super) fn select_rendered_object(
    mut ray_cast: MeshRayCast,
    view: SelectionView,
    selectables: Query<&SelectableObject>,
    mut rendered_materials: Query<&mut MeshMaterial3d<StandardMaterial>>,
    mut state: ResMut<SelectionState>,
    mut editor: ResMut<editor::MapEditor>,
    mut readout: Single<&mut Text, With<SelectionReadout>>,
) {
    // Selecting on release lets a press on the selection start a drag instead.
    if view.teleport_menu.open
        || view.world_map.open
        || view.settings_menu.open
        || view.game_event_menu.open
        || !view.buttons.just_released(MouseButton::Left)
        || editor.is_dragging()
        || editor.pointer_over_panel()
    {
        return;
    }
    let Some(cursor) = view.window.cursor_position() else {
        return;
    };
    let camera = view.camera;
    let Ok(ray) = camera.0.viewport_to_world(camera.1, cursor) else {
        return;
    };
    let selectable = |entity| selectables.contains(entity);
    let settings = MeshRayCastSettings::default()
        .with_filter(&selectable)
        .never_early_exit();
    let hits = ray_cast
        .cast_ray(ray, &settings)
        .iter()
        .map(|(entity, _)| *entity)
        .collect::<Vec<_>>();
    if hits.is_empty() {
        return;
    }
    let backward = view.keys.pressed(KeyCode::ShiftLeft) || view.keys.pressed(KeyCode::ShiftRight);
    let same_stack = state
        .cursor
        .is_some_and(|previous| previous.distance(cursor) <= 4.0)
        && state.hits == hits;
    state.hit_index = selection_hit_index(state.hit_index, hits.len(), same_stack, backward);
    state.cursor = Some(cursor);
    state.hits = hits;
    let entity = state.hits[state.hit_index];
    if let Some((previous, material)) = state.selected.take()
        && let Ok(mut rendered) = rendered_materials.get_mut(previous)
    {
        rendered.0 = material;
    }
    let Ok(selected) = selectables.get(entity) else {
        return;
    };
    editor.selected = selected.export;
    readout.0 = format!(
        "Hit {} / {}\n{}",
        state.hit_index + 1,
        state.hits.len(),
        selection_description(selected)
    );
    // Surfaces drawn with a combined material keep their own look; only the
    // standard material can be swapped for the highlight.
    if let Ok(mut rendered) = rendered_materials.get_mut(entity) {
        let original = std::mem::replace(&mut rendered.0, state.highlight.clone());
        state.selected = Some((entity, original));
    }
}
