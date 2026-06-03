struct VsInput {
    uint instance_id : SV_InstanceID;
    uint vertex_id : SV_VertexID;
};

struct VsOutput {
    float4 clip_position : SV_Position;
    float3 debug_color: Color;
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

static const uint HEIGHT_MAP_INDEX = 1;
static const uint NORMAL_MAP_INDEX = 2;
static const uint TERRAIN_NODE_BUFFER_INDEX = 3;
static const uint CELL_INDEX_BUFFER_INDEX = 4;

static const uint CELL_QUAD_COUNT = 8;
static const uint CELL_VERTEX_COUNT = (CELL_QUAD_COUNT + 1) * (CELL_QUAD_COUNT + 1);
static const uint CELL_TRIANGLE_COUNT = CELL_QUAD_COUNT * CELL_QUAD_COUNT * 2;

static const uint TOP_STITCH_BIT = 1 << 0;
static const uint BOTTOM_STITCH_BIT = 1 << 1;
static const uint LEFT_STITCH_BIT = 1 << 2;
static const uint RIGHT_STITCH_BIT = 1 << 3;
static const uint TOP_LEFT_STITCH_BIT = 1 << 4;
static const uint TOP_RIGHT_STITCH_BIT = 1 << 5;
static const uint BOTTOM_LEFT_STITCH_BIT = 1 << 6;
static const uint BOTTOM_RIGHT_STITCH_BIT = 1 << 7;

VsOutput ProcessVertex(uint vertex_id, uint instance_id) {
    const Texture2D<float> height_map = ResourceDescriptorHeap[HEIGHT_MAP_INDEX];
    const StructuredBuffer<TerrainNode> nodes = ResourceDescriptorHeap[TERRAIN_NODE_BUFFER_INDEX];

    const TerrainNode node = nodes[instance_id];

    uint vx = vertex_id % (CELL_QUAD_COUNT + 1);
    uint vz = vertex_id / (CELL_QUAD_COUNT + 1);
    uint height_mip_index = node.lod_index;

    if (consts.stitching_enabled)
    {
        const bool stitch_x = (vz == 0 && node.stitch_mask & TOP_STITCH_BIT) || (vz == CELL_QUAD_COUNT && node.stitch_mask & BOTTOM_STITCH_BIT);
        const bool stitch_z = (vx == 0 && node.stitch_mask & LEFT_STITCH_BIT) || (vx == CELL_QUAD_COUNT && node.stitch_mask & RIGHT_STITCH_BIT);

        if (stitch_x) {
            vx = (vx / 2) * 2;
        }

        if (stitch_z) {
            vz = (vz / 2) * 2;
        }

        const bool stitch_corner =
            (vx == 0 && vz == 0 && node.stitch_mask & TOP_LEFT_STITCH_BIT) ||
            (vx == CELL_QUAD_COUNT && vz == 0 && node.stitch_mask & TOP_RIGHT_STITCH_BIT)||
            (vx == 0 && vz == CELL_QUAD_COUNT && node.stitch_mask & BOTTOM_LEFT_STITCH_BIT) ||
            (vx == CELL_QUAD_COUNT && vz == CELL_QUAD_COUNT && node.stitch_mask & BOTTOM_RIGHT_STITCH_BIT);

        if (stitch_x || stitch_z || stitch_corner) {
            height_mip_index += 1;
        }
    }

    const float2 local = float2(vx, vz) / (float)CELL_QUAD_COUNT; // 0..1
    const float2 world_xz = node.center + (local - 0.5) * 2.0 * node.half_size;

    const float TILE_WORLD_SIZE = 64.0;
    const float MAX_TILE_COUNT = 32.0;
    const float ATLAS_CENTER_TILE = MAX_TILE_COUNT / 2;

    const float2 world_tile = floor(world_xz / TILE_WORLD_SIZE);
    const float2 atlas_tile = float2(ATLAS_CENTER_TILE, ATLAS_CENTER_TILE) + world_tile - consts.world_center_tile;
    const float2 local_uv = frac(world_xz / TILE_WORLD_SIZE);
    const float2 atlas_uv = (atlas_tile + local_uv) / float(MAX_TILE_COUNT);

    const float height = height_map.SampleLevel(point_clamp_sampler, atlas_uv, 0).r;

    const float3 world_position = float3(
        world_xz.x * consts.world_scale,
        height * 100,// consts.height_scale,
        world_xz.y * consts.world_scale
    );

    VsOutput output = (VsOutput)0;
    output.clip_position = mul(consts.world_to_clip, float4(world_position, 1.0));
    output.debug_color = node_color(node);
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
    out vertices VsOutput vertices[CELL_VERTEX_COUNT],
    out indices uint3 triangles[CELL_TRIANGLE_COUNT]
) {
    SetMeshOutputCounts(CELL_VERTEX_COUNT, CELL_TRIANGLE_COUNT);

    if (gtid < CELL_VERTEX_COUNT) {
        vertices[gtid] = ProcessVertex(gtid, gid);
    }

    const Buffer<uint> index_buffer = ResourceDescriptorHeap[CELL_INDEX_BUFFER_INDEX];

    if (gtid < CELL_TRIANGLE_COUNT) { 
        triangles[gtid] = uint3(
            index_buffer[gtid * 3 + 0],
            index_buffer[gtid * 3 + 1],
            index_buffer[gtid * 3 + 2]
        );
    }
}

float4 ps_main(VsOutput input) : SV_Target {
    const Texture2D<float3> normal_map = ResourceDescriptorHeap[NORMAL_MAP_INDEX];
    const float3 normal = normal_map.Sample(linear_clamp_sampler, input.uv);

    const float3 light_dir = normalize(float3(1.0, 2.0, 1.0));
    const float ndotl = saturate(dot(normal, light_dir));
    const float3 ambient = 0.1;
    
    float3 color = consts.wireframe_pass ? input.debug_color : height_to_color(input.height);

    if (!consts.wireframe_pass) {
        color *= ambient + ndotl;
    }

    return float4(color, 1.0);
}
