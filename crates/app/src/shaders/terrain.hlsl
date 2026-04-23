struct VsInput {
    uint instance_id : SV_InstanceID;
    uint vertex_id : SV_VertexID;
};

struct VsOutput {
    float4 clip_position : SV_Position;
    float3 color : Color;
};

struct Consts {
    float4x4 world_to_clip;
    float4x4 local_to_world;
};

ConstantBuffer<Consts> CONSTS;
SamplerState POINT_CLAMP_SAMPLER : register(s0);

static const uint2 OFFSETS[6] = {
    uint2(0, 0),
    uint2(1, 0),
    uint2(0, 1),
    uint2(1, 0),
    uint2(1, 1),
    uint2(0, 1),
};

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

VsOutput vs_main(VsInput input) {
    const Texture2D<float> height_map = ResourceDescriptorHeap[1];

    uint width;
    uint dummy_height;
    height_map.GetDimensions(width, dummy_height);

    const uint tile_x = input.instance_id % (width - 1);
    const uint tile_z = input.instance_id / (width - 1);

    const uint2 texel = uint2(tile_x, tile_z) + OFFSETS[input.vertex_id];
    const float2 uv = float2(texel) / width;
    const float height = height_map.SampleLevel(POINT_CLAMP_SAMPLER, uv, 0).r;

    const float height_scale = 10.0;
    const float tile_offset = width / 2;
    const float3 world_position = float3((float)texel.x - tile_offset, height * height_scale, (float)texel.y - tile_offset);

    VsOutput output = (VsOutput)0;
    output.clip_position = mul(CONSTS.world_to_clip, float4(world_position, 1.0));
    output.color = height_to_color(height);

    return output;
}

float4 ps_main(VsOutput input) : SV_Target {
    return float4(input.color, 1.0);
}
