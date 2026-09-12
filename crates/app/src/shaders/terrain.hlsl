struct VsInput {
    uint instance_id : SV_InstanceID;
    uint vertex_id : SV_VertexID;
};

struct VsOutput {
    float4 clip_pos : SV_Position;
    float3 normal: Normal;
    float3 debug_color: DebugColor;
    float height : Height;
    float2 uv : Uv;
};

struct TerrainConsts {
    float4x4 world_to_clip;
    float height_scale;
    float elapsed_time;
    uint active_patch_buffer_index;

    // Debug
    uint wireframe_pass;
    uint display_normals;
};

struct TerrainPatch {
    uint2 atlas_slot;
    uint x;
    uint z;
    uint lod;
};

ConstantBuffer<TerrainConsts> consts : register(b0, space1);

SamplerState point_clamp_sampler : register(s0, space0);
SamplerState linear_clamp_sampler : register(s0, space1);

static const uint TERRAIN_BAND_COUNT = 7;

// rgb = band color, w = the height the band is centered on
// Keep w strictly increasing
static const float4 TERRAIN_BANDS[TERRAIN_BAND_COUNT] = {
    float4(0.00, 0.10, 0.40, 0.300), // deep water
    float4(0.10, 0.30, 0.60, 0.350), // shallow water
    float4(0.76, 0.70, 0.50, 0.400), // sand
    float4(0.20, 0.55, 0.10, 0.450), // grass
    float4(0.10, 0.35, 0.05, 0.500), // forest
    float4(0.50, 0.45, 0.40, 0.600), // rock
    float4(0.90, 0.95, 1.00, 0.700), // snow
};

float3 height_to_color(float h) {
    float3 color = TERRAIN_BANDS[0].rgb;

    [unroll]
    for (uint i = 1; i < TERRAIN_BAND_COUNT; ++i) {
        const float t = smoothstep(TERRAIN_BANDS[i - 1].w, TERRAIN_BANDS[i].w, h);
        color = lerp(color, TERRAIN_BANDS[i].rgb, t);
    }

    return color;
}

static const uint HEIGHT_ATLAS_INDEX = 1;
static const uint GRADIENT_ATLAS_INDEX = 2;
static const uint PATCH_INDEX_BUFFER_INDEX = 3;

static const uint PATCH_LOD_COUNT = 11; // must match config.rs
static const uint PATCH_SIZE_IN_METERS = 64;
static const uint PATCH_SIZE_IN_PIXELS = PATCH_SIZE_IN_METERS * 2;
static const uint PATCH_COUNT_PER_SIDE = 1 << (PATCH_LOD_COUNT - 1);
static const uint WORLD_SIZE_IN_METERS = PATCH_SIZE_IN_METERS * PATCH_COUNT_PER_SIDE;

static const uint PATCH_QUAD_COUNT = PATCH_SIZE_IN_PIXELS;
static const uint PATCH_VERTEX_COUNT = (PATCH_QUAD_COUNT + 1) * (PATCH_QUAD_COUNT + 1);
static const uint PATCH_TRIANGLE_COUNT = PATCH_QUAD_COUNT * PATCH_QUAD_COUNT * 2;

static const uint ATLAS_PATCH_SIZE_IN_PIXELS = PATCH_SIZE_IN_PIXELS + 1; // for pixel overlap

// Mirrors `get_lod_color` in terrain/renderer.rs
float3 get_lod_color(uint lod) {
    switch (lod % PATCH_LOD_COUNT) {
        case 0:
            return float3(0.10, 0.80, 0.20); // green
        case 1:
            return float3(0.10, 0.45, 1.00); // blue
        case 2:
            return float3(1.00, 0.80, 0.10); // yellow
        case 3:
            return float3(1.00, 0.30, 0.10); // orange
        case 4:
            return float3(0.75, 0.20, 1.00); // purple
        case 5:
            return float3(0.10, 0.90, 0.90); // cyan
        case 6:
            return float3(1.00, 0.40, 0.70); // pink
        case 7:
            return float3(0.60, 0.60, 0.60); // grey
        case 8:
            return float3(0.95, 0.15, 0.15); // red
        case 9:
            return float3(0.00, 0.55, 0.50); // teal
        case 10:
            return float3(0.85, 0.65, 0.40); // tan
    }

    return 0.0;
}

float3 patch_color(TerrainPatch patch) {
    const float3 lod_color = get_lod_color(patch.lod);
    const bool is_odd_patch = ((patch.x + patch.z) & 1) != 0;
    const float checker_factor = is_odd_patch ? 1.1 : 0.8;

    return saturate(lod_color * checker_factor);
}

VsOutput process_vertex(uint vertex_id, uint instance_id) {
    const StructuredBuffer<TerrainPatch> patches = ResourceDescriptorHeap[consts.active_patch_buffer_index];
    const Texture2D<float> height_atlas = ResourceDescriptorHeap[HEIGHT_ATLAS_INDEX];
    const Texture2D<float2> gradient_atlas = ResourceDescriptorHeap[GRADIENT_ATLAS_INDEX];

    const TerrainPatch patch = patches[instance_id];
    const uint ix = vertex_id % (PATCH_QUAD_COUNT + 1);
    const uint iz = vertex_id / (PATCH_QUAD_COUNT + 1);

    const float2 uv = float2(ix, iz) / (float)PATCH_QUAD_COUNT; // 0..1
    const float size_in_meters = PATCH_SIZE_IN_METERS * (1 << patch.lod);
    const float world_x = patch.x * size_in_meters - WORLD_SIZE_IN_METERS * 0.5 + uv.x * size_in_meters;
    const float world_z = patch.z * size_in_meters - WORLD_SIZE_IN_METERS * 0.5 + uv.y * size_in_meters;

    const uint2 atlas_texel_pos = patch.atlas_slot * ATLAS_PATCH_SIZE_IN_PIXELS + uint2(ix, iz);
    const float height = height_atlas[atlas_texel_pos];
    const float2 gradient = gradient_atlas[atlas_texel_pos];

    const float3 world_pos = float3(world_x, height * consts.height_scale, world_z);

    const float slope_scale = consts.height_scale;
    const float3 normal = normalize(float3(gradient.x * slope_scale, 1.0, gradient.y * slope_scale));

    VsOutput output = (VsOutput)0;
    output.clip_pos = mul(consts.world_to_clip, float4(world_pos, 1.0));
    output.normal = normal;
    output.uv = uv;
    output.height = height;
    output.debug_color = patch_color(patch);

    return output;
}

VsOutput vs_main(VsInput input) {
    return process_vertex(input.vertex_id, input.instance_id);
}

[NumThreads(128, 1, 1)]
[OutputTopology("triangle")]
void ms_main(
    uint gtid : SV_GroupThreadID,
    uint gid : SV_GroupID,
    out vertices VsOutput vertices[PATCH_VERTEX_COUNT],
    out indices uint3 triangles[PATCH_TRIANGLE_COUNT]
) {
    SetMeshOutputCounts(PATCH_VERTEX_COUNT, PATCH_TRIANGLE_COUNT);

    if (gtid < PATCH_VERTEX_COUNT) {
        vertices[gtid] = process_vertex(gtid, gid);
    }

    const Buffer<uint> index_buffer = ResourceDescriptorHeap[PATCH_INDEX_BUFFER_INDEX];

    if (gtid < PATCH_TRIANGLE_COUNT) { 
        triangles[gtid] = uint3(
            index_buffer[gtid * 3 + 0],
            index_buffer[gtid * 3 + 1],
            index_buffer[gtid * 3 + 2]
        );
    }
}

float4 ps_main(VsOutput input) : SV_Target {
    if (consts.wireframe_pass)
        return float4(input.debug_color, 1.0);

    if (consts.display_normals)
        return float4(input.normal * 0.5 + 0.5, 1.0);

    const float sun_speed = 0.5;
    const float sun_angle = consts.elapsed_time * sun_speed;

    const float3 sun_light_dir = normalize(float3(
        cos(sun_angle),
        1.5,
        sin(sun_angle)
    ));

    const float ndotl = saturate(dot(normalize(input.normal), sun_light_dir));
    const float3 ambient = 0.1;
    const float3 color = height_to_color(input.height) * ndotl + ambient;

    return float4(color, 1.0);
}
