/*{
  "DESCRIPTION": "Rotated grid of round dots sized by local brightness.",
  "CATEGORIES": ["Stylize"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "cell", "TYPE": "float", "MIN": 3.0, "MAX": 60.0, "DEFAULT": 12.0 },
    { "NAME": "angle", "TYPE": "float", "MIN": 0.0, "MAX": 6.2831853, "DEFAULT": 0.6 },
    { "NAME": "paper", "TYPE": "color", "DEFAULT": [0.04, 0.04, 0.06, 1.0] },
    { "NAME": "tint_dots", "TYPE": "bool", "DEFAULT": true }
  ]
}*/
void main() {
    vec2 px = isf_FragNormCoord * RENDERSIZE;
    float cs = cos(angle);
    float sn = sin(angle);
    mat2 rot = mat2(cs, -sn, sn, cs);
    vec2 g = rot * px / cell;
    vec2 id = floor(g) + vec2(0.5);
    vec2 centre_px = transpose(rot) * (id * cell);
    vec4 c = IMG_NORM_PIXEL(inputImage, centre_px / RENDERSIZE);
    float l = dot(c.rgb, vec3(0.2126, 0.7152, 0.0722));
    float radius_cells = 0.75 * sqrt(clamp(l, 0.0, 1.0));
    float d = length(fract(g) - 0.5);
    float m = 1.0 - smoothstep(radius_cells - 0.08, radius_cells, d);
    vec3 ink = tint_dots ? c.rgb / max(l, 0.2) : vec3(1.0);
    gl_FragColor = vec4(mix(paper.rgb, clamp(ink, 0.0, 1.0), m), c.a);
}
