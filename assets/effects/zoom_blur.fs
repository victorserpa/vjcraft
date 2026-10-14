/*{
  "DESCRIPTION": "Streaks the image toward or away from a centre point.",
  "CATEGORIES": ["Blur"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "amount", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.2 },
    { "NAME": "center", "TYPE": "point2D", "MIN": [0.0, 0.0], "MAX": [1.0, 1.0], "DEFAULT": [0.5, 0.5] }
  ]
}*/
void main() {
    vec2 p = isf_FragNormCoord - center;
    vec4 sum = vec4(0.0);
    for (int i = 0; i < 16; i++) {
        float s = 1.0 - amount * float(i) / 15.0;
        sum += IMG_NORM_PIXEL(inputImage, center + p * s);
    }
    gl_FragColor = sum / 16.0;
}
