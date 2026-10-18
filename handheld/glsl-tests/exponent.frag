#version 330
out vec4 color;
void main() {
    float e = 1e-3 + 1.0;
    float f = 2.5E+2 * 2;
    float g = 1 - 1e2;
    float h = .5 * 2;
    float i = 2 * .5;
    color = vec4(e, f, g, h + i);
}
