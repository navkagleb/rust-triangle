struct VsInput
{
    float3 m_Position : sem_Position;
    float3 m_Color : sem_Color;
};

struct VsOutput
{
    float4 m_ClipPosition : SV_Position;
    float3 m_Color : Color;
};

VsOutput Main(VsInput input)
{
    VsOutput output = (VsOutput)0;
    output.m_ClipPosition = float4(input.m_Position, 1.0);
    output.m_Color = input.m_Color;

    return output;
}
