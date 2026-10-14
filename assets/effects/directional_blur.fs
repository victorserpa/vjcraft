/*{
  "DESCRIPTION": "Motion blur along a chosen angle.",
  "CATEGORIES": ["Blur"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "length_px", "TYPE": "float", "MIN": 0.0, "MAX": 200.0, "DEFAULT": 30.0 },
    { "NAME": "angle", "TYPE": "float", "MIN": 0.0, "MAX": 6.2831853, "DEFAULT": 0.0 }
  ]
}*/
void main() {
    vec2 dir = vec2(cos(angle), sin(angle)) * length_px / RENDERSIZE;
    vec4 sum = vec4(0.0);
    for (int i = 0; i < 17; i++) {
        float t = float(i) / 16.0 - 0.5;
        sum += IMG_NORM_PIXEL(inputImage, isf_FragNormCoord + dir * t);
    }
    gl_FragColor = sum / 17.0;
}
