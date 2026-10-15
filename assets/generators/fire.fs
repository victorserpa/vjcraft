/*{
  "DESCRIPTION": "Rising flames from the bottom edge.",
  "CATEGORIES": ["Generator"],
  "INPUTS": [
    { "NAME": "inner", "TYPE": "color", "DEFAULT": [1.0, 0.95, 0.6, 1.0] },
    { "NAME": "outer", "TYPE": "color", "DEFAULT": [1.0, 0.25, 0.02, 1.0] },
    { "NAME": "speed", "TYPE": "float", "MIN": 0.0, "MAX": 4.0, "DEFAULT": 1.2 },
    { "NAME": "height", "TYPE": "float", "MIN": 0.2, "MAX": 2.0, "DEFAULT": 1.0 },
    { "NAME": "scale", "TYPE": "float", "MIN": 1.0, "MAX": 12.0, "DEFAULT": 4.0 }
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
    vec2 uv = isf_FragNormCoord;
    vec2 p = vec2(uv.x * aspect, uv.y) * scale;
    p.y -= TIME * speed * 1.5;
    float n = 0.5 * vnoise(p) + 0.3 * vnoise(p * 2.0 + 3.7) + 0.2 * vnoise(p * 4.0 - 1.3);
    float heat = clamp(1.0 - uv.y / height, 0.0, 1.0);
    float flame = clamp(heat * 1.6 + (n - 0.5) * 1.2 - 0.15, 0.0, 1.0);
    vec3 c = mix(vec3(0.0), outer.rgb, smoothstep(0.0, 0.5, flame));
    c = mix(c, inner.rgb, smoothstep(0.5, 1.0, flame));
    gl_FragColor = vec4(c + vec3(0.02, 0.0, 0.0) , 1.0);
}
