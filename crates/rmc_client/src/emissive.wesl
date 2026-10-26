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
//
// Tiles (rmc_client::tiles): the mesher writes each vertex's material id in
// uv.x. tile_lookup.faces[id] holds the tile layer + 1 of the material's
// top, side and bottom faces (0: none). A face with a tile takes its base
// color from the tile array instead of the material color: the texel times
// the vertex color's face shade × AO, recovered as the vertex color's
// largest channel over the material color's (tile_lookup.base[id]). Tile
// coordinates are world position in cells, so one tile covers one cell and
// greedy-merged faces repeat it (the sampler repeats); side tiles keep their
// top edge up. The sample is taken in uniform control flow (mip selection
// uses derivatives) and used only where the face has a tile.

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
// EmissiveFromAlpha::tiles, #[texture(104, dimension = "2d_array")] #[sampler(105)].
@group(constants::MATERIAL_BIND_GROUP) @binding(104) var tile_textures: texture_2d_array<f32>;
@group(constants::MATERIAL_BIND_GROUP) @binding(105) var tile_sampler: sampler;

// rmc_client::tiles::TileLookup (MAX_TILE_MATERIALS entries).
struct TileLookup {
    faces: array<vec4<u32>, 256>,
    base: array<vec4<f32>, 256>,
};
// EmissiveFromAlpha::tile_lookup, #[uniform(106)].
@group(constants::MATERIAL_BIND_GROUP) @binding(106) var<uniform> tile_lookup: TileLookup;

// The tile layer + 1 for a face of material `id` with normal `n`; 0 for none.
fn tile_slot(id: u32, n: vec3<f32>) -> u32 {
    if id >= 256u {
        return 0u;
    }
    let faces = tile_lookup.faces[id];
    if n.y > 0.5 {
        return faces.x;
    }
    if n.y < -0.5 {
        return faces.z;
    }
    return faces.y;
}

// Tile coordinates of world position `p` (in cells) on a face with normal
// `n`: x/z on top and bottom, the horizontal axis and -y on sides.
fn tile_uv(p: vec3<f32>, n: vec3<f32>) -> vec2<f32> {
    if abs(n.y) > 0.5 {
        return p.xz;
    }
    if abs(n.x) > 0.5 {
        return vec2<f32>(p.z * sign(n.x), -p.y);
    }
    return vec2<f32>(-p.x * sign(n.z), -p.y);
}

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
    let n = normalize(in.world_normal);

    // The face's tile, sampled in uniform control flow.
    var slot = 0u;
    @if(VERTEX_UVS_A) {
        slot = tile_slot(u32(in.uv.x + 0.5), n);
    }
    let uv = tile_uv(in.world_position.xyz / jitter_cell_size_m, n);
    let texel = textureSample(tile_textures, tile_sampler, uv, i32(max(slot, 1u) - 1u));
    @if(VERTEX_UVS_A) {
        if slot > 0u {
            let c = pbr_input.material.base_color.rgb;
            let base = tile_lookup.base[u32(in.uv.x + 0.5)].rgb;
            let shade_ao = max(c.r, max(c.g, c.b)) / max(max(base.r, max(base.g, base.b)), 1e-4);
            pbr_input.material.base_color = vec4<f32>(
                texel.rgb * shade_ao,
                pbr_input.material.base_color.a
            );
        }
    }

    // Emissive strength, before alpha_discard makes the opaque alpha 1.
    let strength = pbr_input.material.base_color.a;

    pbr_input.material.base_color = alpha_discard(
        pbr_input.material.flags,
        pbr_input.material.alpha_cutoff,
        pbr_input.material.base_color
    );

    // The cell behind the face, and its brightness factor.
    let inside = in.world_position.xyz - n * jitter_inset_m;
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
