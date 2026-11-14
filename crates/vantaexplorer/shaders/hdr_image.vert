#version 450
// Draws one HDR image quad into the swapchain, beneath the UI layer.
// The rect arrives in NDC so no projection matrix is needed.

layout(push_constant) uniform Params {
    vec4 rect;    // x0, y0, x1, y1 in NDC
    vec4 params;  // scale, sdr, primaries, exposure
} p;

layout(location = 0) out vec2 vUV;

void main() {
    const vec2 corners[6] = vec2[6](
        vec2(0.0, 0.0), vec2(1.0, 0.0), vec2(0.0, 1.0),
        vec2(1.0, 0.0), vec2(1.0, 1.0), vec2(0.0, 1.0)
    );
    vec2 c = corners[gl_VertexIndex];
    vUV = c;
    gl_Position = vec4(mix(p.rect.xy, p.rect.zw, c), 0.0, 1.0);
}
