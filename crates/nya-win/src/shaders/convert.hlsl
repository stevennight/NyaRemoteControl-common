// Desktop -> encoder input (NV12 planes / AYUV / BGRA), with scaling.
// Colour: BT.709, limited range. Full-screen triangle, no vertex buffer.
// Source: 8-bit BGRA (sRGB), or FP16 scRGB on HDR desktops, which is
// tone-mapped to SDR here.

Texture2D<float4> src_tex : register(t0);
SamplerState lin : register(s0);

cbuffer Params : register(b0) {
    float4 src_rect; // u0, v0, u1, v1
    float4 hdr;      // x: 1 = scRGB source; y: factor that maps SDR white to 1.0
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

// Everything up to SDR white passes through unchanged (desktop, text and
// UI must look exactly as on an SDR screen). Brighter HDR highlights are
// scaled back by their largest channel, which keeps their hue instead of
// clipping each channel separately.
float3 tonemap(float3 c) {
    c = max(c * hdr.y, 0.0);   // negative = outside the sRGB gamut
    float m = max(max(c.r, c.g), c.b);
    return m > 1.0 ? c / m : c;
}

float3 srgb_oetf(float3 c) {
    float3 lo = c * 12.92;
    float3 hi = 1.055 * pow(c, 1.0 / 2.4) - 0.055;
    return lerp(lo, hi, step(0.0031308, c));
}

float3 fetch(float2 uv) {
    float3 c = src_tex.Sample(lin, uv).rgb;
    return hdr.x > 0.5 ? srgb_oetf(tonemap(c)) : c;
}

static const float3 KY = float3(0.2126, 0.7152, 0.0722);
static const float3 KU = float3(-0.114572, -0.385428, 0.5);
static const float3 KV = float3(0.5, -0.454153, -0.045847);

float y_of(float3 c) { return 16.0 / 255.0 + dot(c, KY) * (219.0 / 255.0); }
float2 uv_of(float3 c) { return 128.0 / 255.0 + float2(dot(c, KU), dot(c, KV)) * (224.0 / 255.0); }

// NV12 luma plane (R8 view).
float ps_y(VSOut i) : SV_Target { return y_of(fetch(i.uv)); }

// NV12 chroma plane (R8G8 view, half resolution). The bilinear sample at the
// centre of each 2x2 block averages the four source pixels.
float2 ps_uv(VSOut i) : SV_Target { return uv_of(fetch(i.uv)); }

// AYUV (R8G8B8A8 view): R=V, G=U, B=Y, A=alpha.
float4 ps_ayuv(VSOut i) : SV_Target {
    float3 c = fetch(i.uv);
    float2 uv = uv_of(c);
    return float4(uv.y, uv.x, y_of(c), 1.0);
}

// Plain (scaled) copy, e.g. BGRA for NVENC's internal 4:4:4 conversion.
float4 ps_copy(VSOut i) : SV_Target { return float4(fetch(i.uv), 1.0); }
