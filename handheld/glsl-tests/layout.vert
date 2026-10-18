#version 330
layout(location = 0) in vec3 a_pos;
layout(location = 1) in vec2 a_uv;
uniform mat4 u_mvp;
uniform float u_scale[4];
out vec2 v_uv;
void main() {
    v_uv = a_uv * u_scale[2];
    gl_Position = u_mvp * vec4(a_pos, 1);
}
