// BGRA desktop -> encoder input (NV12 planes / AYUV / BGRA), with scaling.
// Colour: BT.709, limited range. Full-screen triangle, no vertex buffer.

Texture2D<float4> src_tex : register(t0);
SamplerState lin : register(s0);

cbuffer Params : register(b0) {
    float4 src_rect; // u0, v0, u1, v1
};

struct VSOut {
    float4 pos : SV_Position;
    float2 uv : TEXCOORD0;
};

VSOut vs_main(uint id : SV_VertexID) {
    VSOut o;
    float2 t = float2((id << 1) & 2, id & 2);
    o.pos = float4(t.x * 2.0 - 1.0, 1.0 - t.y * 2.0, 0.0, 1.0);
    o.uv = src_rect.xy + (src_rect.zw - src_rect.xy) * t;
    return o;
}

static const float3 KY = float3(0.2126, 0.7152, 0.0722);
static const float3 KU = float3(-0.114572, -0.385428, 0.5);
static const float3 KV = float3(0.5, -0.454153, -0.045847);

float y_of(float3 c) { return 16.0 / 255.0 + dot(c, KY) * (219.0 / 255.0); }
float2 uv_of(float3 c) { return 128.0 / 255.0 + float2(dot(c, KU), dot(c, KV)) * (224.0 / 255.0); }

// NV12 luma plane (R8 view).
float ps_y(VSOut i) : SV_Target { return y_of(src_tex.Sample(lin, i.uv).rgb); }

// NV12 chroma plane (R8G8 view, half resolution). The bilinear sample at the
// centre of each 2x2 block averages the four source pixels.
float2 ps_uv(VSOut i) : SV_Target { return uv_of(src_tex.Sample(lin, i.uv).rgb); }

// AYUV (R8G8B8A8 view): R=V, G=U, B=Y, A=alpha.
float4 ps_ayuv(VSOut i) : SV_Target {
    float3 c = src_tex.Sample(lin, i.uv).rgb;
    float2 uv = uv_of(c);
    return float4(uv.y, uv.x, y_of(c), 1.0);
}

// Plain (scaled) copy, e.g. BGRA for NVENC's internal 4:4:4 conversion.
float4 ps_copy(VSOut i) : SV_Target { return float4(src_tex.Sample(lin, i.uv).rgb, 1.0); }
