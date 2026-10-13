/*{
  "DESCRIPTION": "Kaleidoscope with a configurable number of segments.",
  "CATEGORIES": ["Geometry"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "segments", "TYPE": "float", "MIN": 2.0, "MAX": 24.0, "DEFAULT": 6.0 },
    { "NAME": "angle", "TYPE": "float", "MIN": 0.0, "MAX": 6.2831853, "DEFAULT": 0.0 }
  ]
}*/
void main() {
    vec2 p = isf_FragNormCoord - vec2(0.5);
    float r = length(p);
    float a = atan(p.y, p.x) + angle;
    float seg = 6.2831853 / floor(segments);
    a = abs(mod(a, seg) - seg * 0.5);
    vec2 uv = vec2(cos(a), sin(a)) * r + vec2(0.5);
    gl_FragColor = IMG_NORM_PIXEL(inputImage, uv);
}
