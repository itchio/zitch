#version 330
out vec4 color;
float f(float x) { return x * 2.0; }
int f(int x) { return x * 2; }
vec2 g(vec2 v) { return v; }
vec2 g(vec2 v, float s) { return v * s; }
void main() {
    int i = f(1);
    float h = f(1.0);
    vec2 v = g(vec2(1.0), 2.0);
    color = vec4(float(i), h, v);
}
