struct VsOutput
{
    float4 m_ClipPosition : SV_Position;
    float3 m_Normal : Normal;
};

float4 Main(VsOutput input) : SV_Target
{
    return float4(input.m_Normal, 1.0);
}
