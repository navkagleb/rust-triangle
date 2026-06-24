struct VsInput {
    uint instance_id : SV_InstanceID;
    uint vertex_id : SV_VertexID;
};

struct VsOutput {
    float4 clip_position : SV_Position;
    float3 debug_color: DebugColor;
    float height : Height;
    float2 uv : Uv;
};

struct TerrainConsts {
    float4x4 world_to_clip;
    int2 camera_grid_index;
    float terrain_to_world_scale;
    float terrain_height_scale;
    uint wireframe_pass;
    uint stitching_enabled;
    uint active_patch_buffer_index;
};

struct TerrainPatch {
    int2 grid_index;
    uint lod_index;
    uint stitch_mask;
};

ConstantBuffer<TerrainConsts> consts : register(b0, space1);

SamplerState point_clamp_sampler : register(s0, space0);
SamplerState linear_clamp_sampler : register(s0, space1);

float3 height_to_color(float h) {
    float3 deep_water = float3(0.0, 0.1, 0.4);
    float3 shallow = float3(0.1, 0.3, 0.6);
    float3 sand = float3(0.76, 0.7, 0.5);
    float3 grass = float3(0.2, 0.55, 0.1);
    float3 forest = float3(0.1, 0.35, 0.05);
    float3 rock = float3(0.5, 0.45, 0.4);
    float3 snow = float3(0.9, 0.95, 1.0);

    if (h < 0.20)
        return lerp(deep_water, shallow, h / 0.2);

    if (h < 0.25)
        return lerp(shallow, sand, (h - 0.20) / 0.05);

    if (h < 0.35)
        return lerp(sand, grass, (h - 0.25) / 0.10);

    if (h < 0.55)
        return lerp(grass, forest, (h - 0.35) / 0.20);

    if (h < 0.70)
        return lerp(forest, rock, (h - 0.55) / 0.15);

    if (h < 0.85)
        return lerp(rock, snow, (h - 0.70) / 0.15);

    return snow;
}

static const uint INDIRECTION_TEXTURE_INDEX = 1;
static const uint HEIGHT_ATLAS_INDEX = 2;
static const uint PATCH_INDEX_BUFFER_INDEX = 3;

static const uint PATCH_LOD_COUNT = 6;
static const uint PATCH_PIXEL_SIZE = 128;
static const uint PATCH_TERRAIN_SIZE = PATCH_PIXEL_SIZE / 2;
static const uint PATCH_QUAD_COUNT = PATCH_PIXEL_SIZE;
static const uint PATCH_VERTEX_COUNT = (PATCH_QUAD_COUNT + 1) * (PATCH_QUAD_COUNT + 1);
static const uint PATCH_TRIANGLE_COUNT = PATCH_QUAD_COUNT * PATCH_QUAD_COUNT * 2;

static const uint ATLAS_PATCH_PIXEL_SIZE = PATCH_PIXEL_SIZE + 1; // for pixel overlap
static const uint INDIRECTION_SLOT_COUNT = 512;

static const uint TOP_STITCH_BIT = 1 << 0;
static const uint BOTTOM_STITCH_BIT = 1 << 1;
static const uint LEFT_STITCH_BIT = 1 << 2;
static const uint RIGHT_STITCH_BIT = 1 << 3;

float3 get_lod_color(uint lod_index) {
    switch (lod_index % PATCH_LOD_COUNT) {
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
    }

    return 0.0;
}

float3 patch_color(TerrainPatch patch) {
    const float3 lod_color = get_lod_color(patch.lod_index);
    const int2 lod_grid_index = patch.grid_index >> patch.lod_index;
    const bool is_odd_patch = ((lod_grid_index.x + lod_grid_index.y) & 1) != 0;
    const float checker_factor = is_odd_patch ? 1.1 : 0.8;

    return saturate(lod_color * checker_factor);
}

VsOutput process_vertex(uint vertex_id, uint instance_id) {
    const StructuredBuffer<TerrainPatch> patches = ResourceDescriptorHeap[consts.active_patch_buffer_index];
    const Texture2D<uint2> indirection_texture = ResourceDescriptorHeap[INDIRECTION_TEXTURE_INDEX];
    const Texture2D<float> height_atlas = ResourceDescriptorHeap[HEIGHT_ATLAS_INDEX];

    const TerrainPatch patch = patches[instance_id];

    uint ix = vertex_id % (PATCH_QUAD_COUNT + 1);
    uint iz = vertex_id / (PATCH_QUAD_COUNT + 1);

    if (consts.stitching_enabled) {
        const uint mask = patch.stitch_mask;
        const bool stitch_x = (iz == 0 && mask & TOP_STITCH_BIT) || (iz == PATCH_QUAD_COUNT && mask & BOTTOM_STITCH_BIT);
        const bool stitch_z = (ix == 0 && mask & LEFT_STITCH_BIT) || (ix == PATCH_QUAD_COUNT && mask & RIGHT_STITCH_BIT);

        if (stitch_x) {
            ix = (ix / 2) * 2;
        }

        if (stitch_z) {
            iz = (iz / 2) * 2;
        }
    }

    const float2 uv = float2(ix, iz) / (float)PATCH_QUAD_COUNT; // 0..1
    const float terrain_size = PATCH_TERRAIN_SIZE * 1 << patch.lod_index;
    const float2 terrain_xz = patch.grid_index * (int)PATCH_TERRAIN_SIZE + terrain_size * uv;

    const uint lod_index = patch.lod_index;
    const int2 relative_index = (patch.grid_index >> lod_index) - (consts.camera_grid_index >> lod_index);
    const int2 indirection_index = relative_index + (INDIRECTION_SLOT_COUNT >> lod_index) / 2;
    const uint2 atlas_index = indirection_texture.mips[lod_index][indirection_index];

    const float height = height_atlas[atlas_index * ATLAS_PATCH_PIXEL_SIZE + uint2(ix, iz)];

    const float3 world_position = float3(
        terrain_xz.x * consts.terrain_to_world_scale,
        height * consts.terrain_height_scale,
        terrain_xz.y * consts.terrain_to_world_scale
    );

    VsOutput output = (VsOutput)0;
    output.clip_position = mul(consts.world_to_clip, float4(world_position, 1.0));
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
#if 0
    const Texture2D<float3> normal_map = ResourceDescriptorHeap[NORMAL_MAP_INDEX];
    const float3 normal = normal_map.Sample(linear_clamp_sampler, input.uv);

    const float3 light_dir = normalize(float3(1.0, 2.0, 1.0));
    const float ndotl = saturate(dot(normal, light_dir));
    const float3 ambient = 0.1;
#endif
    
    float3 color = consts.wireframe_pass ? input.debug_color : height_to_color(input.height);

    return float4(color, 1.0);
}
