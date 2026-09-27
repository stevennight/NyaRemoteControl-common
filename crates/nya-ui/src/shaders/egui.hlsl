// egui meshes: positions in points, premultiplied gamma-space colours.

cbuffer Params : register(b0) {
    float2 screen_points; // viewport size in egui points
    float2 _pad;
};

Texture2D tex : register(t0);
SamplerState samp : register(s0);

struct VSIn {
    float2 pos : POSITION;
    float2 uv : TEXCOORD0;
    float4 color : COLOR0;
};

struct VSOut {
    float4 pos : SV_Position;
    float2 uv : TEXCOORD0;
    float4 color : COLOR0;
};

VSOut vs_main(VSIn i) {
    VSOut o;
    o.pos = float4(i.pos.x / screen_points.x * 2.0 - 1.0, 1.0 - i.pos.y / screen_points.y * 2.0, 0.0, 1.0);
    o.uv = i.uv;
    o.color = i.color;
    return o;
}

float4 ps_main(VSOut i) : SV_Target {
    return i.color * tex.Sample(samp, i.uv);
}
