use bevy::pbr::{ExtendedMaterial, MaterialExtension};
use bevy::render::render_resource::{
    AsBindGroup, ShaderType, TextureViewDescriptor, TextureViewDimension,
};
use bevy::shader::ShaderRef;

use super::*;

/// An Unreal terrain: every layer is a tiling texture painted over the ones
/// before it through an alpha map stretched across the whole terrain.
pub(super) type TerrainMaterial = ExtendedMaterial<StandardMaterial, TerrainExtension>;

/// As many layers as the shader blends; Lineage terrains use up to nine.
pub(super) const MAX_TERRAIN_LAYERS: usize = 16;

// Every layer texture is resampled to one size so they fit a texture array.
const LAYER_TEXTURE_SIZE: u32 = 512;
// Alpha maps are resampled to the largest one, up to this size.
const MAX_ALPHA_MAP_SIZE: u32 = 1024;

pub(super) struct TerrainPlugin;

impl Plugin for TerrainPlugin {
    fn build(&self, app: &mut App) {
        bevy::asset::embedded_asset!(app, "terrain.wgsl");
        app.add_plugins(MaterialPlugin::<TerrainMaterial>::default());
    }
}

#[derive(Asset, AsBindGroup, Reflect, Debug, Clone)]
pub(super) struct TerrainExtension {
    #[uniform(100)]
    pub(super) layers: TerrainLayers,
    #[texture(101, dimension = "2d_array")]
    #[sampler(102)]
    pub(super) textures: Handle<Image>,
    #[texture(103, dimension = "2d_array")]
    #[sampler(104)]
    pub(super) alpha_maps: Handle<Image>,
}

#[derive(Clone, Copy, Debug, Default, Reflect, ShaderType)]
pub(super) struct TerrainLayers {
    /// The terrain's vertex count along each axis, which the alpha maps span.
    pub(super) size: Vec2,
    pub(super) count: u32,
    /// How often each layer's texture repeats across one terrain quad.
    pub(super) repeats: [Vec4; MAX_TERRAIN_LAYERS],
}

impl MaterialExtension for TerrainExtension {
    fn fragment_shader() -> ShaderRef {
        "embedded://unreal_viewer/terrain.wgsl".into()
    }
}

/// A terrain layer ready to draw: its texture, its alpha map and its tiling.
pub(super) struct LoadedTerrainLayer<'a> {
    pub(super) texture: &'a TextureGeometry,
    pub(super) alpha_map: Option<&'a TextureGeometry>,
    pub(super) repeats: Vec2,
}

/// How often a layer's texture repeats across one quad. Unreal tiles a layer
/// texture once every UScale texels' worth of world units.
pub(super) fn layer_repeats(
    quad_size: Vec2,
    layer: &TerrainLayer,
    texture: &TextureGeometry,
) -> Vec2 {
    let tile = layer.scale * Vec2::new(texture.width as f32, texture.height as f32);
    if tile.x.abs() <= f32::EPSILON || tile.y.abs() <= f32::EPSILON {
        return Vec2::ONE;
    }
    quad_size / tile
}

pub(super) fn terrain_extension(
    terrain_size: Vec2,
    layers: &[LoadedTerrainLayer],
    images: &mut Assets<Image>,
) -> TerrainExtension {
    let layers = &layers[..layers.len().min(MAX_TERRAIN_LAYERS)];
    let mut repeats = [Vec4::ONE; MAX_TERRAIN_LAYERS];
    for (slot, layer) in repeats.iter_mut().zip(layers) {
        *slot = layer.repeats.extend(0.0).extend(0.0);
    }
    let textures: Vec<_> = layers.iter().map(|layer| layer.texture).collect();
    let alpha_size = layers
        .iter()
        .filter_map(|layer| layer.alpha_map)
        .map(|map| map.width.max(map.height))
        .max()
        .unwrap_or(1)
        .min(MAX_ALPHA_MAP_SIZE);
    let alpha_maps: Vec<_> = layers
        .iter()
        .enumerate()
        .map(|(index, layer)| match layer.alpha_map {
            Some(map) => alpha_channel(map, alpha_size),
            // Unreal draws a layer without an alpha map everywhere, which
            // only makes sense for the first one.
            None => vec![if index == 0 { u8::MAX } else { 0 }; (alpha_size * alpha_size) as usize],
        })
        .collect();
    TerrainExtension {
        layers: TerrainLayers {
            size: terrain_size,
            count: layers.len() as u32,
            repeats,
        },
        textures: images.add(layer_texture_array(&textures)),
        alpha_maps: images.add(alpha_map_array(alpha_size, alpha_maps)),
    }
}

fn layer_texture_array(textures: &[&TextureGeometry]) -> Image {
    let size = LAYER_TEXTURE_SIZE;
    let mip_count = size.ilog2() + 1;
    // Unreal terrains tile their textures hundreds of times, which only
    // holds together at a distance with mipmaps.
    let mut data = Vec::new();
    for texture in textures.iter().copied().chain(
        // A texture array needs at least one layer.
        textures.is_empty().then_some(&TextureGeometry {
            width: 1,
            height: 1,
            rgba: vec![u8::MAX; 4],
        }),
    ) {
        let mut level = resample_rgba(texture, size);
        let mut level_size = size;
        data.extend_from_slice(&level);
        while level_size > 1 {
            level = halve_rgba(&level, level_size);
            level_size /= 2;
            data.extend_from_slice(&level);
        }
    }
    // `Image::new` checks the data against the base level only, but the data
    // carries every mip level.
    let mut image = Image::new_uninit(
        Extent3d {
            width: size,
            height: size,
            depth_or_array_layers: textures.len().max(1) as u32,
        },
        TextureDimension::D2,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    );
    image.data = Some(data);
    image.texture_descriptor.mip_level_count = mip_count;
    image.texture_view_descriptor = Some(TextureViewDescriptor {
        dimension: Some(TextureViewDimension::D2Array),
        ..default()
    });
    image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::Repeat,
        address_mode_v: ImageAddressMode::Repeat,
        mag_filter: bevy::image::ImageFilterMode::Linear,
        min_filter: bevy::image::ImageFilterMode::Linear,
        mipmap_filter: bevy::image::ImageFilterMode::Linear,
        anisotropy_clamp: 8,
        ..default()
    });
    image
}

fn alpha_map_array(size: u32, maps: Vec<Vec<u8>>) -> Image {
    let layers = maps.len().max(1) as u32;
    let mut data: Vec<u8> = maps.into_iter().flatten().collect();
    data.resize((size * size * layers) as usize, 0);
    let mut image = Image::new(
        Extent3d {
            width: size,
            height: size,
            depth_or_array_layers: layers,
        },
        TextureDimension::D2,
        data,
        TextureFormat::R8Unorm,
        RenderAssetUsages::RENDER_WORLD,
    );
    image.texture_view_descriptor = Some(TextureViewDescriptor {
        dimension: Some(TextureViewDimension::D2Array),
        ..default()
    });
    image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::ClampToEdge,
        address_mode_v: ImageAddressMode::ClampToEdge,
        mag_filter: bevy::image::ImageFilterMode::Linear,
        min_filter: bevy::image::ImageFilterMode::Linear,
        ..default()
    });
    image
}

/// A layer's weight across the terrain, which Unreal keeps in the alpha map's
/// alpha channel.
fn alpha_channel(map: &TextureGeometry, size: u32) -> Vec<u8> {
    (0..size * size)
        .map(|index| {
            let x = (index % size) * map.width / size;
            let y = (index / size) * map.height / size;
            map.rgba[((y * map.width + x) * 4 + 3) as usize]
        })
        .collect()
}

fn resample_rgba(texture: &TextureGeometry, size: u32) -> Vec<u8> {
    let mut rgba = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let source_x = x * texture.width / size;
            let source_y = y * texture.height / size;
            let offset = ((source_y * texture.width + source_x) * 4) as usize;
            rgba.extend_from_slice(&texture.rgba[offset..offset + 4]);
        }
    }
    rgba
}

fn halve_rgba(level: &[u8], size: u32) -> Vec<u8> {
    let half = size / 2;
    let mut next = Vec::with_capacity((half * half * 4) as usize);
    for y in 0..half {
        for x in 0..half {
            for channel in 0..4 {
                let texel = |dx: u32, dy: u32| {
                    u32::from(level[(((y * 2 + dy) * size + x * 2 + dx) * 4 + channel) as usize])
                };
                next.push(((texel(0, 0) + texel(1, 0) + texel(0, 1) + texel(1, 1) + 2) / 4) as u8);
            }
        }
    }
    next
}
