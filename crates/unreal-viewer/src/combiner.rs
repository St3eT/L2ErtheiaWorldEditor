use bevy::pbr::{ExtendedMaterial, MaterialExtension};
use bevy::render::render_resource::{AsBindGroup, ShaderType};
use bevy::shader::ShaderRef;

use super::*;

/// An Unreal Engine.Combiner: the standard material draws the first material
/// and the extension blends in a second one, each scrolling on its own.
pub(super) type CombinedMaterial = ExtendedMaterial<StandardMaterial, CombinerExtension>;

pub(super) struct CombinerPlugin;

impl Plugin for CombinerPlugin {
    fn build(&self, app: &mut App) {
        bevy::asset::embedded_asset!(app, "combiner.wgsl");
        app.add_plugins(MaterialPlugin::<CombinedMaterial>::default())
            .init_resource::<CombinedMaterials>()
            .add_systems(Update, move_combined_layers);
    }
}

#[derive(Asset, AsBindGroup, Reflect, Debug, Clone)]
pub(super) struct CombinerExtension {
    #[uniform(100)]
    pub(super) layer: CombinerLayer,
    #[texture(101)]
    #[sampler(102)]
    pub(super) second: Handle<Image>,
}

#[derive(Clone, Copy, Debug, Default, Reflect, ShaderType)]
pub(super) struct CombinerLayer {
    pub(super) offset: Vec2,
    pub(super) operation: u32,
    pub(super) modulate: f32,
    /// One when the layers blend by the mesh's vertex alpha.
    pub(super) vertex_mask: u32,
}

impl MaterialExtension for CombinerExtension {
    fn fragment_shader() -> ShaderRef {
        "embedded://unreal_viewer/combiner.wgsl".into()
    }
}

/// Combined materials and how each of their two layers moves.
#[derive(Resource, Default)]
pub(super) struct CombinedMaterials(pub(super) Vec<(Handle<CombinedMaterial>, UvMotion, UvMotion)>);

pub(super) fn combined_material(
    base: StandardMaterial,
    second: &SecondLayer,
    second_image: Handle<Image>,
) -> CombinedMaterial {
    CombinedMaterial {
        base,
        extension: CombinerExtension {
            layer: CombinerLayer {
                offset: Vec2::ZERO,
                operation: second.operation,
                modulate: second.modulate,
                vertex_mask: u32::from(second.vertex_mask),
            },
            second: second_image,
        },
    }
}

// The two layers of a combined material slide against each other, which is
// what makes Lineage's water read as moving even at slow panner rates.
fn move_combined_layers(
    time: Res<Time>,
    combined: Res<CombinedMaterials>,
    mut materials: ResMut<Assets<CombinedMaterial>>,
) {
    let seconds = time.elapsed_secs();
    for (handle, first, second) in &combined.0 {
        let Some(mut material) = materials.get_mut(handle) else {
            continue;
        };
        material.base.uv_transform.translation = first.offset(seconds);
        material.extension.layer.offset = second.offset(seconds);
    }
}
