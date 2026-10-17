#import bevy_pbr::{
    pbr_fragment::pbr_input_from_standard_material,
    pbr_functions::alpha_discard,
}

#ifdef PREPASS_PIPELINE
#import bevy_pbr::{
    prepass_io::{VertexOutput, FragmentOutput},
    pbr_deferred_functions::deferred_output,
}
#else
#import bevy_pbr::{
    forward_io::{VertexOutput, FragmentOutput},
    pbr_functions::{apply_pbr_lighting, main_pass_post_lighting_processing},
    pbr_types::STANDARD_MATERIAL_FLAGS_UNLIT_BIT,
}
#endif

struct TerrainLayers {
    size: vec2<f32>,
    count: u32,
    repeats: array<vec4<f32>, 16>,
}

@group(#{MATERIAL_BIND_GROUP}) @binding(100) var<uniform> terrain: TerrainLayers;
@group(#{MATERIAL_BIND_GROUP}) @binding(101) var layer_textures: texture_2d_array<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(102) var layer_sampler: sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(103) var alpha_maps: texture_2d_array<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(104) var alpha_sampler: sampler;

@fragment
fn fragment(
    in: VertexOutput,
    @builtin(front_facing) is_front: bool,
) -> FragmentOutput {
    var pbr_input = pbr_input_from_standard_material(in, is_front);

    // The mesh carries each vertex's grid position: the alpha maps span the
    // whole grid, while the layer textures repeat across every quad.
    let mask_uv = in.uv / terrain.size;
    var color = vec3(0.0);
    for (var layer = 0u; layer < terrain.count; layer++) {
        let weight = textureSample(alpha_maps, alpha_sampler, mask_uv, layer).r;
        let texel = textureSample(
            layer_textures,
            layer_sampler,
            in.uv * terrain.repeats[layer].xy,
            layer,
        );
        color = mix(color, texel.rgb, weight);
    }
    pbr_input.material.base_color = vec4(
        color * pbr_input.material.base_color.rgb,
        pbr_input.material.base_color.a,
    );
    pbr_input.material.base_color = alpha_discard(pbr_input.material, pbr_input.material.base_color);

#ifdef PREPASS_PIPELINE
    let out = deferred_output(in, pbr_input);
#else
    var out: FragmentOutput;
    if (pbr_input.material.flags & STANDARD_MATERIAL_FLAGS_UNLIT_BIT) == 0u {
        out.color = apply_pbr_lighting(pbr_input);
    } else {
        out.color = pbr_input.material.base_color;
    }
    out.color = main_pass_post_lighting_processing(pbr_input, out.color);
#endif

    return out;
}
