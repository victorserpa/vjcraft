/*{
  "DESCRIPTION": "Cloner Radial: copies of the picture spaced around a circle, optionally turned to face outward and spinning.",
  "CATEGORIES": ["Geometry"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "count", "TYPE": "float", "MIN": 1.0, "MAX": 16.0, "DEFAULT": 6.0 },
    { "NAME": "radius", "TYPE": "float", "MIN": 0.0, "MAX": 0.5, "DEFAULT": 0.25 },
    { "NAME": "size", "TYPE": "float", "MIN": 0.05, "MAX": 1.0, "DEFAULT": 0.3 },
    { "NAME": "start", "TYPE": "float", "MIN": -180.0, "MAX": 180.0, "DEFAULT": 0.0 },
    { "NAME": "spin", "TYPE": "float", "MIN": -90.0, "MAX": 90.0, "DEFAULT": 0.0 },
    { "NAME": "face_out", "TYPE": "bool", "DEFAULT": true }
  ]
}*/
void main() {
    float aspect = RENDERSIZE.x / RENDERSIZE.y;
    vec2 p = (isf_FragNormCoord - 0.5) * vec2(aspect, 1.0);
    float n = floor(count);
    vec3 col = vec3(0.0);
    for (int j = 0; j < 16; j++) {
        int i = 15 - j;
        float fi = float(i);
        if (fi >= n) { continue; }
        float a = radians(start + spin * TIME) + 6.2831853 * fi / n;
        vec2 centre = radius * vec2(cos(a), sin(a));
        vec2 d = (p - centre) / max(size, 0.02);
        if (face_out) {
            // Turn the copy so its top points away from the middle.
            float r = a - 1.5707963;
            d = vec2(cos(r) * d.x + sin(r) * d.y, -sin(r) * d.x + cos(r) * d.y);
        }
        d.x /= aspect;
        vec2 uv = d + 0.5;
        if (uv.x >= 0.0 && uv.x <= 1.0 && uv.y >= 0.0 && uv.y <= 1.0) {
            vec4 c = IMG_NORM_PIXEL(inputImage, uv);
            col = mix(col, c.rgb, c.a);
        }
    }
    gl_FragColor = vec4(col, 1.0);
}
