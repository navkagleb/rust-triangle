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
};

ConstantBuffer<FrameConsts> g_FrameConsts;

VsOutput Main(VsInput input)
{
    VsOutput output = (VsOutput)0;
    output.m_ClipPosition = mul(g_FrameConsts.m_WorldToClip, float4(input.m_Position, 1.0));
    output.m_Normal = input.m_Normal;

    return output;
}
