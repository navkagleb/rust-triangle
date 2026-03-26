struct VsInput {
    float3 pos : sem_Position;
    float3 normal : sem_Normal;
};

struct VsOutput {
    float4 clip_pos : SV_Position;
    float3 normal : Normal;
};

struct FrameConsts {
    float4x4 world_to_clip;
    float4x4 local_to_world;
};

ConstantBuffer<FrameConsts> FRAME_CONSTS;

VsOutput vs_main(VsInput input) {
    VsOutput output = (VsOutput)0;
    output.clip_pos = mul(FRAME_CONSTS.local_to_world, float4(input.pos, 1.0));
    output.clip_pos = mul(FRAME_CONSTS.world_to_clip, output.clip_pos);
    output.normal = normalize(mul((float3x3)FRAME_CONSTS.local_to_world, input.normal));

    return output;
}

float4 ps_main(VsOutput input) : SV_Target {
    const float3 color = input.normal * 0.5 + 0.5;
    return float4(color, 1.0);
}
