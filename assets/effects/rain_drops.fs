/*{
  "DESCRIPTION": "Water droplets on glass that refract the picture and slowly slide, appear and fade.",
  "CATEGORIES": ["Distort"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "size", "TYPE": "float", "MIN": 2.0, "MAX": 20.0, "DEFAULT": 7.0 },
    { "NAME": "strength", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.5 },
    { "NAME": "speed", "TYPE": "float", "MIN": 0.0, "MAX": 3.0, "DEFAULT": 0.5 },
    { "NAME": "coverage", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.7 }
  ]
}*/
float h2_(vec2 p) {
    return fract(sin(dot(p, vec2(127.1, 311.7))) * 43758.5453);
}
vec2 drops_(vec2 uv, float aspect, float scl, float seed) {
    vec2 g = vec2(scl * aspect, scl);
    vec2 p = uv * g;
    vec2 id = floor(p);
    vec2 f = fract(p) - 0.5;
    float r0 = h2_(id + seed);
    float r1 = h2_(id + seed + 17.0);
    float r2 = h2_(id + seed + 41.0);
    float life = fract(TIME * speed * 0.15 + r0);
    float grow = smoothstep(0.0, 0.2, life) * (1.0 - smoothstep(0.7, 1.0, life));
    vec2 centre = (vec2(r1, r2) - 0.5) * 0.5 + vec2(0.0, -0.2 * life);
    vec2 d = f - centre;
    float rad = 0.18 * grow * step(1.0 - coverage, r0 * 0.999 + 0.0005);
    float dist = length(d);
    float inside = 1.0 - smoothstep(rad * 0.85, rad + 1e-4, dist);
    vec2 n = d / max(rad, 1e-3);
    return -n * inside * (1.0 - dist / max(rad, 1e-3)) * rad / g.y * 6.0;
}
void main() {
    float aspect = RENDERSIZE.x / RENDERSIZE.y;
    vec2 uv = isf_FragNormCoord;
    vec2 o = drops_(uv, aspect, size, 0.0) + drops_(uv + 0.31, aspect, size * 1.7, 5.0) * 0.7;
    gl_FragColor = IMG_NORM_PIXEL(inputImage, uv + o * strength * 6.0);
}
