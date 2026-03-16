struct VsInput
{
    float3 m_Position : sem_Position;
    float3 m_Normal : sem_Normal;
};

struct VsOutput
{
    float4 m_ClipPosition : SV_Position;
    float3 m_Normal : Normal;
};

struct FrameConsts
{
    float4x4 m_WorldToClip;
    float4x4 m_LocalToWorld;
};

ConstantBuffer<FrameConsts> g_FrameConsts;

VsOutput VsMain(VsInput input)
{
    VsOutput output = (VsOutput)0;
    output.m_ClipPosition = mul(g_FrameConsts.m_LocalToWorld, float4(input.m_Position, 1.0));
    output.m_ClipPosition = mul(g_FrameConsts.m_WorldToClip, output.m_ClipPosition);
    output.m_Normal = normalize(mul((float3x3)g_FrameConsts.m_LocalToWorld, input.m_Normal));

    return output;
}

float4 PsMain(VsOutput input) : SV_Target
{
    const float3 color = input.m_Normal * 0.5 + 0.5;
    return float4(color, 1.0);
}
