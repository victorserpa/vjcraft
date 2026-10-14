/*{
  "DESCRIPTION": "Offsets pixels by a drifting value-noise field.",
  "CATEGORIES": ["Distort"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "amount", "TYPE": "float", "MIN": 0.0, "MAX": 0.3, "DEFAULT": 0.04 },
    { "NAME": "scale", "TYPE": "float", "MIN": 0.5, "MAX": 40.0, "DEFAULT": 6.0 },
    { "NAME": "speed", "TYPE": "float", "MIN": 0.0, "MAX": 4.0, "DEFAULT": 0.4 }
  ]
}*/
float hash(vec2 p) {
    return fract(sin(dot(p, vec2(127.1, 311.7))) * 43758.5453);
}
float vnoise(vec2 p) {
    vec2 i = floor(p);
    vec2 f = fract(p);
    f = f * f * (3.0 - 2.0 * f);
    return mix(mix(hash(i), hash(i + vec2(1.0, 0.0)), f.x),
               mix(hash(i + vec2(0.0, 1.0)), hash(i + vec2(1.0, 1.0)), f.x), f.y);
}
void main() {
    vec2 p = isf_FragNormCoord * scale;
    float t = TIME * speed;
    vec2 d = vec2(vnoise(p + vec2(t, 0.0)), vnoise(p + vec2(17.3, 9.1 - t))) - 0.5;
    gl_FragColor = IMG_NORM_PIXEL(inputImage, isf_FragNormCoord + d * 2.0 * amount);
}
