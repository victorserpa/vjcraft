/*{
  "DESCRIPTION": "Keeps a horizontal band sharp and blurs above and below it.",
  "CATEGORIES": ["Blur"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "focus", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.5 },
    { "NAME": "band", "TYPE": "float", "MIN": 0.0, "MAX": 0.5, "DEFAULT": 0.12 },
    { "NAME": "strength", "TYPE": "float", "MIN": 0.0, "MAX": 24.0, "DEFAULT": 8.0 }
  ]
}*/
void main() {
    float d = max(abs(isf_FragNormCoord.y - focus) - band, 0.0);
    float r = strength * smoothstep(0.0, 0.4, d);
    vec2 px = vec2(1.0) / RENDERSIZE * r / 2.0;
    vec4 sum = vec4(0.0);
    for (int x = -2; x <= 2; x++) {
        for (int y = -2; y <= 2; y++) {
            sum += IMG_NORM_PIXEL(inputImage, isf_FragNormCoord + vec2(float(x), float(y)) * px);
        }
    }
    gl_FragColor = sum / 25.0;
}
