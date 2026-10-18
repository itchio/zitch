#version 330
out vec4 color;
void main() {
    uint u = 1u;
    uint v = 2u * 3u;
    int h = 0x1F;
    float f = 1.5f * 2;
    float g = 2 * 1.5F;
    color = vec4(float(u + v), float(h), f, g);
}
