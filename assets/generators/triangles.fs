/*{
  "DESCRIPTION": "Tessellated triangles that shimmer between two colours.",
  "CATEGORIES": ["Generator"],
  "INPUTS": [
    { "NAME": "colorA", "TYPE": "color", "DEFAULT": [0.1, 0.1, 0.4, 1.0] },
    { "NAME": "colorB", "TYPE": "color", "DEFAULT": [0.9, 0.4, 0.7, 1.0] },
    { "NAME": "cells", "TYPE": "float", "MIN": 2.0, "MAX": 40.0, "DEFAULT": 8.0 },
    { "NAME": "speed", "TYPE": "float", "MIN": 0.0, "MAX": 4.0, "DEFAULT": 0.8 },
    { "NAME": "edge", "TYPE": "float", "MIN": 0.0, "MAX": 0.2, "DEFAULT": 0.03 }
  ]
}*/
float hash(vec2 p) {
    return fract(sin(dot(p, vec2(127.1, 311.7))) * 43758.5453);
}
void main() {
    float aspect = RENDERSIZE.x / RENDERSIZE.y;
    vec2 g = isf_FragNormCoord * vec2(aspect, 1.0) * cells;
    vec2 id = floor(g);
    vec2 f = fract(g);
    // Each square is split along a diagonal into two triangles.
    float upper = step(f.x + f.y, 1.0);
    float tri = hash(id + upper * 7.31);
    float k = 0.5 + 0.5 * sin(TIME * speed + tri * 6.2831853);
    vec3 c = mix(colorA.rgb, colorB.rgb, mix(tri, k, 0.6));
    float dEdge = abs(f.x + f.y - 1.0) * 0.7071;
    float line = 1.0 - smoothstep(0.0, edge + 0.001, dEdge);
    gl_FragColor = vec4(mix(c, vec3(1.0), line * 0.6 * step(0.0001, edge)), 1.0);
}
