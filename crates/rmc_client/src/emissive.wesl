// Chunk fragment shader: StandardMaterial lighting, plus emission read from
// the vertex color's alpha (SAD §11.2). Loaded as
// embedded://rmc_client/emissive.wesl by rmc_client::material.
//
// The mesher writes vertex color = linear material color × AO, with the
// material's emissive strength in alpha. The base color is white, so after
// pbr_input_from_standard_material the base color is exactly the vertex
// color. The alpha is read before alpha_discard sets it to 1 (the material
// is opaque), and `color.rgb × color.a × emissive_scale` is added to the
// emissive term, so a glowing cell glows in its own color, darkened by AO
// like the rest of its face. A strength of 0 changes nothing: such a
// fragment is shaded exactly as by the StandardMaterial.

import bevy_pbr::render::{
    forward_io::{VertexOutput, FragmentOutput},
    pbr_fragment::pbr_input_from_standard_material,
    pbr_functions::{alpha_discard, apply_pbr_lighting, main_pass_post_lighting_processing},
    pbr_types::STANDARD_MATERIAL_FLAGS_UNLIT_BIT,
};

// EmissiveFromAlpha::emissive_scale, #[uniform(100)].
@group(constants::MATERIAL_BIND_GROUP) @binding(100) var<uniform> emissive_scale: f32;

@fragment
fn fragment(in: VertexOutput, @builtin(front_facing) is_front: bool) -> FragmentOutput {
    var pbr_input = pbr_input_from_standard_material(in, is_front);

    // Emissive strength, before alpha_discard makes the opaque alpha 1.
    let strength = pbr_input.material.base_color.a;

    pbr_input.material.base_color = alpha_discard(
        pbr_input.material.flags,
        pbr_input.material.alpha_cutoff,
        pbr_input.material.base_color
    );

    let glow = pbr_input.material.base_color.rgb * strength * emissive_scale;
    pbr_input.material.emissive = vec4<f32>(
        pbr_input.material.emissive.rgb + glow,
        pbr_input.material.emissive.a
    );

    var out: FragmentOutput;
    if (pbr_input.material.flags & STANDARD_MATERIAL_FLAGS_UNLIT_BIT) == 0u {
        out.color = apply_pbr_lighting(pbr_input);
    } else {
        out.color = pbr_input.material.base_color;
    }
    out.color = main_pass_post_lighting_processing(pbr_input, out.color);
    return out;
}
