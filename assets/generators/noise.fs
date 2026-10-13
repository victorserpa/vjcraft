/*{
  "DESCRIPTION": "Animated value noise between two colors.",
  "CATEGORIES": ["Generator"],
  "INPUTS": [
    { "NAME": "colorA", "TYPE": "color", "DEFAULT": [0.0, 0.0, 0.0, 1.0] },
    { "NAME": "colorB", "TYPE": "color", "DEFAULT": [1.0, 1.0, 1.0, 1.0] },
    { "NAME": "scale", "TYPE": "float", "MIN": 1.0, "MAX": 64.0, "DEFAULT": 8.0 },
    { "NAME": "speed", "TYPE": "float", "MIN": 0.0, "MAX": 4.0, "DEFAULT": 0.5 }
  ]
}*/
float hash(vec2 p) {
    return fract(sin(dot(p, vec2(127.1, 311.7))) * 43758.5453);
}
float vnoise(vec2 p) {
    vec2 i = floor(p);
    vec2 f = fract(p);
    f = f * f * (3.0 - 2.0 * f);
    float a = hash(i);
    float b = hash(i + vec2(1.0, 0.0));
    float c = hash(i + vec2(0.0, 1.0));
    float d = hash(i + vec2(1.0, 1.0));
    return mix(mix(a, b, f.x), mix(c, d, f.x), f.y);
}
void main() {
    vec2 p = isf_FragNormCoord * vec2(scale * RENDERSIZE.x / RENDERSIZE.y, scale)
        + vec2(0.0, TIME * speed);
    gl_FragColor = mix(colorA, colorB, vnoise(p));
}
