/*{
  "DESCRIPTION": "Cloner Linear: a row of copies of the picture along a direction, each one smaller and fainter than the last.",
  "CATEGORIES": ["Geometry"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "count", "TYPE": "float", "MIN": 1.0, "MAX": 16.0, "DEFAULT": 5.0 },
    { "NAME": "spacing", "TYPE": "float", "MIN": 0.0, "MAX": 0.5, "DEFAULT": 0.12 },
    { "NAME": "angle", "TYPE": "float", "MIN": -180.0, "MAX": 180.0, "DEFAULT": 0.0 },
    { "NAME": "size", "TYPE": "float", "MIN": 0.1, "MAX": 1.0, "DEFAULT": 0.45 },
    { "NAME": "taper", "TYPE": "float", "MIN": 0.0, "MAX": 0.9, "DEFAULT": 0.1 },
    { "NAME": "fade", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.3 }
  ]
}*/
void main() {
    float aspect = RENDERSIZE.x / RENDERSIZE.y;
    vec2 p = (isf_FragNormCoord - 0.5) * vec2(aspect, 1.0);
    float n = floor(count);
    vec2 dir = vec2(cos(radians(angle)), sin(radians(angle)));
    vec3 col = vec3(0.0);
    // Back to front: the last copy is drawn first, copy 0 on top.
    for (int j = 0; j < 16; j++) {
        int i = 15 - j;
        float fi = float(i);
        if (fi >= n) { continue; }
        float t = fi / max(n - 1.0, 1.0);
        float s = size * (1.0 - taper * t);
        vec2 centre = dir * spacing * (fi - (n - 1.0) * 0.5) * 2.0;
        vec2 q = (p - centre) / max(s, 0.02);
        q.x /= aspect;
        vec2 uv = q + 0.5;
        if (uv.x >= 0.0 && uv.x <= 1.0 && uv.y >= 0.0 && uv.y <= 1.0) {
            vec4 c = IMG_NORM_PIXEL(inputImage, uv);
            col = mix(col, c.rgb * (1.0 - fade * t), c.a);
        }
    }
    gl_FragColor = vec4(col, 1.0);
}
