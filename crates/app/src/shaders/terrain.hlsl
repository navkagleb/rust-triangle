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

struct FrameConsts {
    float4x4 world_to_clip;
    float4x4 local_to_world;
};

struct TerrainConsts {
    float terrain_size;
    float world_scale;
    float height_scale;
    bool is_wireframe;
};

struct TerrainNode {
    float2 center;
    float half_size;
    uint lod_index;
    uint stitch_mask;
};

ConstantBuffer<FrameConsts> frame_consts;
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
    const uint x = (uint)(node.center.x / node.half_size);
    const uint z = (uint)(node.center.y / node.half_size);
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
static const uint CHUNK_INDEX_BUFFER_INDEX = 4;

static const uint CHUNK_QUAD_COUNT = 8;
static const uint CHUNK_VERTEX_COUNT = (CHUNK_QUAD_COUNT + 1) * (CHUNK_QUAD_COUNT + 1);
static const uint CHUNK_TRIANGLE_COUNT = CHUNK_QUAD_COUNT * CHUNK_QUAD_COUNT * 2;

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

    uint vx = vertex_id % (CHUNK_QUAD_COUNT + 1);
    uint vz = vertex_id / (CHUNK_QUAD_COUNT + 1);

    const bool stitch_x = (vz == 0 && node.stitch_mask & TOP_STITCH_BIT) || (vz == CHUNK_QUAD_COUNT && node.stitch_mask & BOTTOM_STITCH_BIT);
    const bool stitch_z = (vx == 0 && node.stitch_mask & LEFT_STITCH_BIT) || (vx == CHUNK_QUAD_COUNT && node.stitch_mask & RIGHT_STITCH_BIT);
    const bool corner_stitch =
        (vx == 0 && vz == 0 && node.stitch_mask & TOP_LEFT_STITCH_BIT) ||
        (vx == CHUNK_QUAD_COUNT && vz == 0 && node.stitch_mask & TOP_RIGHT_STITCH_BIT)||
        (vx == 0 && vz == CHUNK_QUAD_COUNT && node.stitch_mask & BOTTOM_LEFT_STITCH_BIT) ||
        (vx == CHUNK_QUAD_COUNT && vz == CHUNK_QUAD_COUNT && node.stitch_mask & BOTTOM_RIGHT_STITCH_BIT);

    if (stitch_x) {
        vx = (vx / 2) * 2;
    }

    if (stitch_z) {
        vz = (vz / 2) * 2;
    }

    const float2 local = float2(vx, vz) / (float)CHUNK_QUAD_COUNT; // 0..1
    const float2 world_xz = node.center + (local - 0.5) * node.half_size * 2.0;

    const float2 uv = world_xz / consts.terrain_size;
    const float lod_index = stitch_x || stitch_z || corner_stitch ? node.lod_index + 1 : node.lod_index;
    const float height = height_map.SampleLevel(point_clamp_sampler, uv, lod_index).r;

    const float3 world_position = float3(
        world_xz.x * consts.world_scale,
        height * consts.height_scale,
        world_xz.y * consts.world_scale
    );

    VsOutput output = (VsOutput)0;
    output.clip_position = mul(frame_consts.world_to_clip, float4(world_position, 1.0));
    output.debug_color = node_color(node);
    output.lod_index = node.lod_index;
    output.uv = uv;
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
    out vertices VsOutput vertices[CHUNK_VERTEX_COUNT],
    out indices uint3 triangles[CHUNK_TRIANGLE_COUNT]
) {
    SetMeshOutputCounts(CHUNK_VERTEX_COUNT, CHUNK_TRIANGLE_COUNT);

    if (gtid < CHUNK_VERTEX_COUNT) {
        vertices[gtid] = ProcessVertex(gtid, gid);
    }

    const Buffer<uint> index_buffer = ResourceDescriptorHeap[CHUNK_INDEX_BUFFER_INDEX];

    if (gtid < CHUNK_TRIANGLE_COUNT) { 
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
    
    float3 color = !consts.is_wireframe ? height_to_color(input.height) : input.debug_color;
    color *= ambient + ndotl;

    return float4(color, 1.0);
}
