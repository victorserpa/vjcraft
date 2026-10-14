/*{
  "DESCRIPTION": "Wraps the image around a centre point (rectangular to polar), or unwraps it.",
  "CATEGORIES": ["Distort"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "unwrap", "TYPE": "bool", "DEFAULT": false },
    { "NAME": "spin", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.0 }
  ]
}*/
void main() {
    vec2 p = isf_FragNormCoord - vec2(0.5);
    vec2 uv;
    if (unwrap) {
        float a = atan(p.y, p.x) / 6.2831853 + 0.5 + spin;
        uv = vec2(fract(a), clamp(length(p) * 2.0, 0.0, 1.0));
    } else {
        float a = (isf_FragNormCoord.x + spin) * 6.2831853;
        float r = isf_FragNormCoord.y * 0.5;
        uv = vec2(0.5) + vec2(cos(a), sin(a)) * r;
    }
    gl_FragColor = IMG_NORM_PIXEL(inputImage, uv);
}
