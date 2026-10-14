/*{
  "DESCRIPTION": "Curved glass tube with phosphor mask, scanlines and corner falloff.",
  "CATEGORIES": ["Stylize"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "curvature", "TYPE": "float", "MIN": 0.0, "MAX": 0.5, "DEFAULT": 0.15 },
    { "NAME": "scan", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.35 },
    { "NAME": "mask_strength", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.3 },
    { "NAME": "corner", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.5 }
  ]
}*/
void main() {
    vec2 p = isf_FragNormCoord * 2.0 - 1.0;
    p *= 1.0 + curvature * dot(p, p) * 0.5;
    vec2 uv = p * 0.5 + 0.5;
    float inside = step(0.0, uv.x) * step(uv.x, 1.0) * step(0.0, uv.y) * step(uv.y, 1.0);
    vec4 c = IMG_NORM_PIXEL(inputImage, uv);
    float line = 0.5 + 0.5 * sin(uv.y * RENDERSIZE.y * 3.14159);
    vec3 m = vec3(1.0);
    float col = mod(floor(isf_FragNormCoord.x * RENDERSIZE.x), 3.0);
    m = vec3(col < 0.5 ? 1.0 : 1.0 - mask_strength,
             (col > 0.5 && col < 1.5) ? 1.0 : 1.0 - mask_strength,
             col > 1.5 ? 1.0 : 1.0 - mask_strength);
    float v = 1.0 - corner * smoothstep(0.6, 1.5, length(p));
    vec3 o = c.rgb * m * (1.0 - scan * line) * v * 1.1;
    gl_FragColor = vec4(o * inside, c.a);
}
