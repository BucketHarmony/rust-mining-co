// Chunk fragment shader: StandardMaterial lighting, plus emission read from
// the vertex color's alpha (SAD §11.2). Loaded as
// embedded://rmc_client/emissive.wesl by rmc_client::material.
//
// The mesher writes vertex color = linear material color × face shade × AO,
// with the material's emissive strength in alpha. The base color is white,
// so after pbr_input_from_standard_material the base color is exactly the
// vertex color. The alpha is read before alpha_discard sets it to 1 (the
// material is opaque), and `color.rgb × color.a × emissive_scale` is added
// to the emissive term, so a glowing cell glows in its own color, darkened
// by its face shade and AO like the rest of its face. A strength of 0 adds
// nothing: apart from the cell jitter below, such a fragment is shaded
// exactly as by the StandardMaterial.
//
// Surface variation (F097): each cell's base color is scaled by
// `cell_jitter(cell)`, a factor in 1 ± jitter_amplitude from a hash of the
// cell the fragment lies on, taken jitter_inset_m behind the face along its
// normal. The mesh and its greedy merging are unchanged. rmc_client::material
// mirrors cell_hash and cell_jitter as plain Rust functions for the tests.

import bevy_pbr::render::{
    forward_io::{VertexOutput, FragmentOutput},
    pbr_fragment::pbr_input_from_standard_material,
    pbr_functions::{alpha_discard, apply_pbr_lighting, main_pass_post_lighting_processing},
    pbr_types::STANDARD_MATERIAL_FLAGS_UNLIT_BIT,
};

// EmissiveFromAlpha::emissive_scale, #[uniform(100)].
@group(constants::MATERIAL_BIND_GROUP) @binding(100) var<uniform> emissive_scale: f32;
// EmissiveFromAlpha::jitter_amplitude, jitter_cell_size_m, jitter_inset_m.
@group(constants::MATERIAL_BIND_GROUP) @binding(101) var<uniform> jitter_amplitude: f32;
@group(constants::MATERIAL_BIND_GROUP) @binding(102) var<uniform> jitter_cell_size_m: f32;
@group(constants::MATERIAL_BIND_GROUP) @binding(103) var<uniform> jitter_inset_m: f32;

// rmc_client::material::cell_hash: CELL_HASH_MULTIPLIERS per axis, then the
// CELL_HASH_MIXERS finalizer.
fn cell_hash(cell: vec3<i32>) -> u32 {
    let c = bitcast<vec3<u32>>(cell);
    var h = ((c.x * 0x8da6b343u) ^ (c.y * 0xd8163841u)) ^ (c.z * 0xcb1ab31fu);
    h = h ^ (h >> 16u);
    h = h * 0x7feb352du;
    h = h ^ (h >> 15u);
    h = h * 0x846ca68bu;
    h = h ^ (h >> 16u);
    return h;
}

// rmc_client::material::cell_jitter: the hash's top 24 bits mapped to
// 1 ± jitter_amplitude.
fn cell_jitter(cell: vec3<i32>) -> f32 {
    let unit = f32(cell_hash(cell) >> 8u) / 16777215.0;
    return 1.0 + jitter_amplitude * (2.0 * unit - 1.0);
}

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

    // The cell behind the face, and its brightness factor.
    let inside = in.world_position.xyz - normalize(in.world_normal) * jitter_inset_m;
    let cell = vec3<i32>(floor(inside / jitter_cell_size_m));
    let shade = cell_jitter(cell);
    pbr_input.material.base_color = vec4<f32>(
        pbr_input.material.base_color.rgb * shade,
        pbr_input.material.base_color.a
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
