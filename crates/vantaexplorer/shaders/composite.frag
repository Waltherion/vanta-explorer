#version 450
// Composites the egui UI layer onto whatever the HDR content pass already wrote.
//
// The UI layer arrives as LINEAR, premultiplied-alpha fp16: egui-ash-renderer is
// initialised with srgb_framebuffer=true, so its own vertex shader has already done
// sRGB->linear and its fragment shader passes linear through. That is why there is no
// srgbToLinear here.
//
// uiScale carries the whole "how bright is UI white" question:
//   HDR out: ui.nits / 80.0   (windows-scRGB: 1.0 = 80 nits), 203 nits => 2.5375
//   SDR out: 1.0
// uiSat compensates for the fact that a tagged scRGB surface bypasses Hyprland's
// sdrsaturation, so our chrome would otherwise read flatter than the untagged apps
// sitting next to it.

layout(location = 0) in vec2 vUV;
layout(location = 0) out vec4 fragColor;

layout(set = 0, binding = 0) uniform sampler2D uUi;

layout(push_constant) uniform Params {
    float uiScale;
    float uiSat;
    float sdr;      // 1.0 = SDR swapchain (we encode sRGB ourselves), 0.0 = scRGB linear
    float _pad;
} p;

vec3 srgbEncode(vec3 c) {
    vec3 lo = 12.92 * c;
    vec3 hi = 1.055 * pow(max(c, vec3(1e-5)), vec3(1.0 / 2.4)) - 0.055;
    return mix(lo, hi, step(vec3(0.0031308), c));
}

vec3 applySaturation(vec3 c, float s) {
    float l = dot(c, vec3(0.2126, 0.7152, 0.0722));
    return mix(vec3(l), c, s);
}

void main() {
    vec4 ui = texture(uUi, vUV);

    // Unpremultiply so saturation/encoding operate on the straight colour.
    vec3 straight = ui.a > 0.001 ? ui.rgb / ui.a : vec3(0.0);
    straight = applySaturation(straight, p.uiSat) * p.uiScale;

    if (p.sdr > 0.5) {
        straight = srgbEncode(clamp(straight, 0.0, 1.0));
    }

    // Re-premultiply: the pipeline blends with ONE / ONE_MINUS_SRC_ALPHA.
    fragColor = vec4(straight * ui.a, ui.a);
}
