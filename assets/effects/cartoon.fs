/*{
  "DESCRIPTION": "Cartoon: flat colour bands with dark outlines.",
  "CATEGORIES": ["Stylize"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "levels", "TYPE": "float", "MIN": 2.0, "MAX": 12.0, "DEFAULT": 5.0 },
    { "NAME": "edge", "TYPE": "float", "MIN": 0.0, "MAX": 10.0, "DEFAULT": 4.0 },
    { "NAME": "edge_cut", "TYPE": "float", "MIN": 0.05, "MAX": 1.0, "DEFAULT": 0.3 }
  ]
}*/
float lum(vec2 uv) {
    return dot(IMG_NORM_PIXEL(inputImage, uv).rgb, vec3(0.299, 0.587, 0.114));
}
void main() {
    vec2 uv = isf_FragNormCoord;
    vec2 px = 1.0 / RENDERSIZE;
    vec4 c = IMG_THIS_PIXEL(inputImage);
    vec3 q = floor(c.rgb * levels + 0.5) / levels;
    float gx = lum(uv + vec2(px.x, 0.0)) - lum(uv - vec2(px.x, 0.0));
    float gy = lum(uv + vec2(0.0, px.y)) - lum(uv - vec2(0.0, px.y));
    float e = smoothstep(edge_cut * 0.5, edge_cut, length(vec2(gx, gy)) * edge);
    gl_FragColor = vec4(mix(q, vec3(0.0), e), c.a);
}
