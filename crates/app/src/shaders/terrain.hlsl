struct VsInput {
    uint instance_id : SV_InstanceID;
    uint vertex_id : SV_VertexID;
};

struct VsOutput {
    float4 clip_position : SV_Position;
    float3 debug_color: Color;
    float3 debug_color2: Color2;
    nointerpolation uint lod_index : LodIndex;
    float2 uv : Uv;
    float height : Height;
};

struct TerrainConsts {
    float4x4 world_to_clip;
    float2 world_center_tile;
    float world_scale;
    float height_scale;
    uint wireframe_pass;
    uint stitching_enabled;
};

struct TerrainNode {
    float2 center;
    float half_size;
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

float3 lod_to_color(uint lod) {
    switch (lod) {
        case 0: return float3(1.0, 0.0, 0.0); // red
        case 1: return float3(1.0, 0.5, 0.0); // orange
        case 2: return float3(1.0, 1.0, 0.0); // yellow
        case 3: return float3(0.0, 1.0, 0.0); // green
        case 4: return float3(0.0, 1.0, 1.0); // cyan
        case 5: return float3(0.0, 0.0, 1.0); // blue
        case 6: return float3(0.5, 0.0, 1.0); // purple
    }

    return float3(1.0, 1.0, 1.0); // white
}

float3 hsv_to_rgb(float h, float s, float v) {
    const float4 K = float4(1.0, 2.0 / 3.0, 1.0 / 3.0, 3.0);
    const float3 p = abs(frac(h + K.xyz) * 6.0 - K.www);

    return v * lerp(K.xxx, saturate(p - K.xxx), s);
}

float3 node_color(TerrainNode node) {
    const int x = (int)(node.center.x / node.half_size);
    const int z = (int)(node.center.y / node.half_size);
    const uint lod = node.lod_index;

    uint hash = x * 73856093u;
    hash = hash ^ (z * 19349663u);
    hash = hash ^ (lod * 83492791u);
    hash ^= hash >> 17;
    hash *= 0xbf324c81u;
    hash ^= hash >> 11;
    hash *= 0x9a812d7du;
    hash ^= hash >> 15;

    const float hue = frac((float)(hash & 0xFFFF) / 65535.0 + 0.618033988);
    return hsv_to_rgb(hue, 0.75 + 0.25 * frac((float)(hash >> 16) / 65535.0), 0.9);
}

static const uint INDIRECTION_TEXTURE_INDEX = 1;
static const uint HEIGHT_ATLAS_INDEX = 2;
static const uint NORMAL_ATLAS_INDEX = 3;
static const uint TERRAIN_NODE_BUFFER_INDEX = 4;
static const uint TILE_INDEX_BUFFER_INDEX = 5;

static const uint TILE_QUAD_COUNT = 8;
static const uint TILE_VERTEX_COUNT = (TILE_QUAD_COUNT + 1) * (TILE_QUAD_COUNT + 1);
static const uint TILE_TRIANGLE_COUNT = TILE_QUAD_COUNT * TILE_QUAD_COUNT * 2;

static const uint TOP_STITCH_BIT = 1 << 0;
static const uint BOTTOM_STITCH_BIT = 1 << 1;
static const uint LEFT_STITCH_BIT = 1 << 2;
static const uint RIGHT_STITCH_BIT = 1 << 3;
static const uint TOP_LEFT_STITCH_BIT = 1 << 4;
static const uint TOP_RIGHT_STITCH_BIT = 1 << 5;
static const uint BOTTOM_LEFT_STITCH_BIT = 1 << 6;
static const uint BOTTOM_RIGHT_STITCH_BIT = 1 << 7;

static const float TILE_WORLD_SIZE = 64.0;
static const uint MAX_TILE_COUNT = 32;

VsOutput ProcessVertex(uint vertex_id, uint instance_id) {
    const Texture2DArray<uint2> indirection_texture = ResourceDescriptorHeap[INDIRECTION_TEXTURE_INDEX];
    const Texture2D<float> height_atlas = ResourceDescriptorHeap[HEIGHT_ATLAS_INDEX];
    const StructuredBuffer<TerrainNode> nodes = ResourceDescriptorHeap[TERRAIN_NODE_BUFFER_INDEX];

    const TerrainNode node = nodes[instance_id];

    uint vx = vertex_id % (TILE_QUAD_COUNT + 1);
    uint vz = vertex_id / (TILE_QUAD_COUNT + 1);
    uint lod_index = node.lod_index;

    if (consts.stitching_enabled)
    {
        const bool stitch_x = (vz == 0 && node.stitch_mask & TOP_STITCH_BIT) || (vz == TILE_QUAD_COUNT && node.stitch_mask & BOTTOM_STITCH_BIT);
        const bool stitch_z = (vx == 0 && node.stitch_mask & LEFT_STITCH_BIT) || (vx == TILE_QUAD_COUNT && node.stitch_mask & RIGHT_STITCH_BIT);

        if (stitch_x) {
            vx = (vx / 2) * 2;
        }

        if (stitch_z) {
            vz = (vz / 2) * 2;
        }

        const bool stitch_corner =
            (vx == 0 && vz == 0 && node.stitch_mask & TOP_LEFT_STITCH_BIT) ||
            (vx == TILE_QUAD_COUNT && vz == 0 && node.stitch_mask & TOP_RIGHT_STITCH_BIT)||
            (vx == 0 && vz == TILE_QUAD_COUNT && node.stitch_mask & BOTTOM_LEFT_STITCH_BIT) ||
            (vx == TILE_QUAD_COUNT && vz == TILE_QUAD_COUNT && node.stitch_mask & BOTTOM_RIGHT_STITCH_BIT);

        if (stitch_x || stitch_z || stitch_corner) {
            lod_index += 1;
        }
    }

    const float2 tile_uv_bad = float2(vx, vz) / (float)TILE_QUAD_COUNT; // 0..1
    const float2 world_xz = node.center + node.half_size * (tile_uv_bad - 0.5) * 2.0;

#if 0
    const float2 tile_uv = float2(vx, vz) / ((float)TILE_QUAD_COUNT + 0.0001);

    const int2 world_tile = (node.center - node.half_size) / TILE_WORLD_SIZE;
    const int2 relative_tile = world_tile - consts.world_center_tile;
    const int2 indirection_coord = relative_tile + MAX_TILE_COUNT / 2;
    const float2 atlas_tile = indirection_texture[uint3(indirection_coord.x, indirection_coord.y, node.lod_index)];

    const float2 atlas_uv = (atlas_tile + tile_uv) / float(MAX_TILE_COUNT);
#else
    int2 world_tile = (node.center - node.half_size) / TILE_WORLD_SIZE;
    float2 tile_uv = tile_uv_bad;

    if (vx == TILE_QUAD_COUNT) {
        world_tile.x += 1;
        tile_uv.x = 0.0;
    }

    if (vz == TILE_QUAD_COUNT) {
        world_tile.y += 1;
        tile_uv.y = 0.0;
    }

    const int2 relative_tile = world_tile - int2(consts.world_center_tile);
    const int2 indirection_coord = relative_tile + MAX_TILE_COUNT / 2;
    const float2 atlas_tile = indirection_texture[uint3(indirection_coord.x, indirection_coord.y, node.lod_index)];

    const float2 atlas_uv = (atlas_tile + tile_uv) / float(MAX_TILE_COUNT);
#endif
    const float height = height_atlas.SampleLevel(point_clamp_sampler, atlas_uv, 0).r;

    const float3 world_position = float3(
        world_xz.x * consts.world_scale,
        height * 100.0,
        world_xz.y * consts.world_scale
    );

    VsOutput output = (VsOutput)0;
    output.clip_position = mul(consts.world_to_clip, float4(world_position, 1.0));
    output.debug_color = node_color(node);
    output.debug_color2 = float3(tile_uv, 0.0);
    output.debug_color2 = float3(height * 0.5 + 0.5, 0.0, 0.0);
    // output.debug_color2 = float3(float2(world_tile) / 24.0, 0.0);
    output.lod_index = node.lod_index;
    output.uv = atlas_uv;
    output.height = height;

    return output;
}

VsOutput vs_main(VsInput input) {
    return ProcessVertex(input.vertex_id, input.instance_id);
}

[NumThreads(128, 1, 1)]
[OutputTopology("triangle")]
void ms_main(
    uint gtid : SV_GroupThreadID,
    uint gid : SV_GroupID,
    out vertices VsOutput vertices[TILE_VERTEX_COUNT],
    out indices uint3 triangles[TILE_TRIANGLE_COUNT]
) {
    SetMeshOutputCounts(TILE_VERTEX_COUNT, TILE_TRIANGLE_COUNT);

    if (gtid < TILE_VERTEX_COUNT) {
        vertices[gtid] = ProcessVertex(gtid, gid);
    }

    const Buffer<uint> index_buffer = ResourceDescriptorHeap[TILE_INDEX_BUFFER_INDEX];

    if (gtid < TILE_TRIANGLE_COUNT) { 
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

    // if (!consts.wireframe_pass) {
    //     color *= ambient + ndotl;
    // }

    return float4(color, 1.0);
}
