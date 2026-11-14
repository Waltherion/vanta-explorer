#version 450
// Present stage for HDR content. Adapted from vantaviewer's image.frag so the proven colour
// path is reused: the texture is fp16 LINEAR with 1.0 = 203 nits and NATIVE primaries.
//
// Note the scale here is always 203/80, the content must NOT follow the chrome's
// matchDesktop compensation, or HDR images would be exposed wrongly.

layout(location = 0) in vec2 vUV;
layout(location = 0) out vec4 fragColor;

layout(set = 0, binding = 0) uniform sampler2D uImage;

layout(push_constant) uniform Params {
    vec4 rect;
    vec4 params;  // x = scale (203/80), y = sdr flag, z = primaries, w = exposure
} p;

vec3 bt2020ToBt709(vec3 c) {
    return vec3(
        1.660491 * c.r - 0.587641 * c.g - 0.072850 * c.b,
       -0.124550 * c.r + 1.132900 * c.g - 0.008349 * c.b,
       -0.018151 * c.r - 0.100579 * c.g + 1.118730 * c.b);
}

vec3 p3ToBt709(vec3 c) {
    return vec3(
        1.224940 * c.r - 0.224940 * c.g,
       -0.042056 * c.r + 1.042056 * c.g,
       -0.019637 * c.r - 0.078636 * c.g + 1.098273 * c.b);
}

vec3 srgbEncode(vec3 c) {
    vec3 lo = 12.92 * c;
    vec3 hi = 1.055 * pow(max(c, vec3(1e-5)), vec3(1.0 / 2.4)) - 0.055;
    return mix(lo, hi, step(vec3(0.0031308), c));
}

void main() {
    vec4 texel = texture(uImage, vUV);
    vec3 c = texel.rgb * p.params.w;

    int prim = int(p.params.z + 0.5);
    if (prim == 1) c = bt2020ToBt709(c);
    else if (prim == 2) c = p3ToBt709(c);
    // Out-of-gamut negatives would read as odd colours; clamp for safe display.
    c = max(c, vec3(0.0));

    if (p.params.y > 0.5) {
        // SDR output: Reinhard with a white point (Lw = 6), identical to the CPU path in
        // vanta_hdr::color::tonemap_reinhard so thumbnails and preview agree.
        float L = dot(c, vec3(0.2126, 0.7152, 0.0722));
        if (L > 1e-4) {
            const float Lw = 6.0;
            float Lt = L * (1.0 + L / (Lw * Lw)) / (1.0 + L);
            c *= Lt / L;
        }
        fragColor = vec4(srgbEncode(clamp(c, 0.0, 1.0)), texel.a);
    } else {
        fragColor = vec4(c * p.params.x, texel.a);
    }
}
