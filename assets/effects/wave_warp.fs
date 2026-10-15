/*{
  "DESCRIPTION": "Two-dimensional sine warp: both axes ripple at once.",
  "CATEGORIES": ["Distort"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "amp_x", "TYPE": "float", "MIN": 0.0, "MAX": 0.2, "DEFAULT": 0.02 },
    { "NAME": "amp_y", "TYPE": "float", "MIN": 0.0, "MAX": 0.2, "DEFAULT": 0.02 },
    { "NAME": "freq_x", "TYPE": "float", "MIN": 0.5, "MAX": 40.0, "DEFAULT": 6.0 },
    { "NAME": "freq_y", "TYPE": "float", "MIN": 0.5, "MAX": 40.0, "DEFAULT": 8.0 },
    { "NAME": "speed", "TYPE": "float", "MIN": -8.0, "MAX": 8.0, "DEFAULT": 1.5 }
  ]
}*/
void main() {
    vec2 uv = isf_FragNormCoord;
    float t = TIME * speed;
    vec2 w = vec2(
        amp_x * sin(uv.y * freq_y + t),
        amp_y * sin(uv.x * freq_x + t * 1.3));
    w += 0.5 * vec2(
        amp_x * sin((uv.x + uv.y) * freq_x * 0.7 - t * 0.8),
        amp_y * cos((uv.x - uv.y) * freq_y * 0.7 + t * 0.6));
    gl_FragColor = IMG_NORM_PIXEL(inputImage, uv + w);
}
