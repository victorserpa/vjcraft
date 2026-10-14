/*{
  "DESCRIPTION": "Stars streaming toward the viewer in several depth layers.",
  "CATEGORIES": ["Generator"],
  "INPUTS": [
    { "NAME": "space", "TYPE": "color", "DEFAULT": [0.0, 0.0, 0.03, 1.0] },
    { "NAME": "starlight", "TYPE": "color", "DEFAULT": [0.9, 0.95, 1.0, 1.0] },
    { "NAME": "density", "TYPE": "float", "MIN": 2.0, "MAX": 40.0, "DEFAULT": 14.0 },
    { "NAME": "speed", "TYPE": "float", "MIN": 0.0, "MAX": 2.0, "DEFAULT": 0.25 },
    { "NAME": "star_size", "TYPE": "float", "MIN": 0.02, "MAX": 0.3, "DEFAULT": 0.1 },
    { "NAME": "streak", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.0 }
  ]
}*/
float hash(vec2 p) {
    return fract(sin(dot(p, vec2(127.1, 311.7))) * 43758.5453);
}
void main() {
    float aspect = RENDERSIZE.x / RENDERSIZE.y;
    vec2 p = (isf_FragNormCoord - 0.5) * vec2(aspect, 1.0);
    vec3 col = space.rgb;
    for (int layer = 0; layer < 4; layer++) {
        float z = fract(float(layer) * 0.25 + TIME * speed);
        float zoom = mix(4.0, 0.3, z);
        vec2 g = p * density * zoom + vec2(float(layer) * 17.0, float(layer) * 5.0);
        vec2 id = floor(g);
        vec2 f = fract(g) - 0.5;
        vec2 o = vec2(hash(id), hash(id + 7.0)) - 0.5;
        vec2 d = f - o * 0.7;
        d.y *= 1.0 / (1.0 + streak * 6.0 * z);
        float r = star_size * (0.4 + z);
        float b = (1.0 - smoothstep(0.0, r, length(d))) * hash(id + 3.0) * smoothstep(0.0, 0.15, z) * (1.0 - smoothstep(0.8, 1.0, z));
        col += starlight.rgb * b;
    }
    gl_FragColor = vec4(clamp(col, 0.0, 1.0), 1.0);
}
