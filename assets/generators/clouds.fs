/*{
  "DESCRIPTION": "Drifting fractal-noise clouds.",
  "CATEGORIES": ["Generator"],
  "INPUTS": [
    { "NAME": "sky", "TYPE": "color", "DEFAULT": [0.15, 0.4, 0.8, 1.0] },
    { "NAME": "cloud", "TYPE": "color", "DEFAULT": [1.0, 1.0, 1.0, 1.0] },
    { "NAME": "scale", "TYPE": "float", "MIN": 0.5, "MAX": 10.0, "DEFAULT": 3.0 },
    { "NAME": "speed", "TYPE": "float", "MIN": 0.0, "MAX": 2.0, "DEFAULT": 0.15 },
    { "NAME": "coverage", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.5 }
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
    float aspect = RENDERSIZE.x / RENDERSIZE.y;
    vec2 p = isf_FragNormCoord * vec2(aspect, 1.0) * scale + vec2(TIME * speed, TIME * speed * 0.3);
    float f = 0.0;
    float a = 0.5;
    for (int i = 0; i < 5; i++) {
        f += a * vnoise(p);
        p = p * 2.03 + vec2(17.0, 9.0);
        a *= 0.5;
    }
    float d = smoothstep(1.0 - coverage - 0.15, 1.0 - coverage + 0.25, f);
    gl_FragColor = vec4(mix(sky.rgb, cloud.rgb, d), 1.0);
}
