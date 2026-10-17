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

struct CombinerLayer {
    offset: vec2<f32>,
    operation: u32,
    modulate: f32,
    vertex_mask: u32,
}

@group(#{MATERIAL_BIND_GROUP}) @binding(100) var<uniform> layer: CombinerLayer;
@group(#{MATERIAL_BIND_GROUP}) @binding(101) var second_texture: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(102) var second_sampler: sampler;

// Unreal's EColorOperation, mixing the first material, drawn by the standard
// material underneath, with the second layer sampled here.
fn combine(first: vec3<f32>, second: vec4<f32>) -> vec3<f32> {
    switch layer.operation {
        case 1u: { return second.rgb; }
        case 2u: { return first * second.rgb; }
        case 3u: { return first + second.rgb; }
        case 4u: { return first - second.rgb; }
        case 5u: { return mix(first, second.rgb, second.a); }
        default: { return first; }
    }
}

@fragment
fn fragment(
    in: VertexOutput,
    @builtin(front_facing) is_front: bool,
) -> FragmentOutput {
    var pbr_input = pbr_input_from_standard_material(in, is_front);

    // Unreal's combiners run on fixed-function hardware, which saturates.
    var second = textureSample(second_texture, second_sampler, in.uv + layer.offset);
#ifdef VERTEX_COLORS
    // A combiner masked by Engine.VertexColor blends by the alpha painted
    // onto the mesh, which the standard material has already multiplied in.
    if layer.vertex_mask == 1u {
        second.a = in.color.a;
        pbr_input.material.base_color.a = 1.0;
    }
#endif
    let combined = combine(pbr_input.material.base_color.rgb, second) * layer.modulate;
    pbr_input.material.base_color = vec4(
        clamp(combined, vec3(0.0), vec3(1.0)),
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
