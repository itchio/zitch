#version 330
in vec3 v_normal;
in vec2 v_uv;
uniform sampler2D u_tex;
out vec4 color;
float Map(float v, float a, float b, float c, float d) {
    return c + (v - a) * (d - c) / (b - a);
}
vec3 shade(vec3 n, float k) {
    return n * k;
}
void main() {
    float m = Map(v_normal.z, 50, 0, 0, 1);
    float x = 1;
    float y = x * 2 + 0.5;
    vec3 n = shade(v_normal, 2);
    int count = 3;
    color = texture(u_tex, v_uv) * vec4(n, m) * y / float(count);
}
