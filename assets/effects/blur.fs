/*{
  "DESCRIPTION": "Simple box blur.",
  "CATEGORIES": ["Blur"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "radius", "TYPE": "float", "MIN": 0.0, "MAX": 16.0, "DEFAULT": 4.0 }
  ]
}*/
void main() {
    vec2 step = vec2(1.0) / RENDERSIZE * radius / 4.0;
    vec4 sum = vec4(0.0);
    for (int x = -4; x <= 4; x++) {
        for (int y = -4; y <= 4; y++) {
            sum += IMG_NORM_PIXEL(inputImage, isf_FragNormCoord + vec2(float(x), float(y)) * step);
        }
    }
    gl_FragColor = sum / 81.0;
}
