/*{
  "DESCRIPTION": "Smooth bell-weighted blur over a 9x9 neighbourhood.",
  "CATEGORIES": ["Blur"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "radius", "TYPE": "float", "MIN": 0.0, "MAX": 32.0, "DEFAULT": 6.0 }
  ]
}*/
void main() {
    vec2 px = vec2(1.0) / RENDERSIZE * radius / 4.0;
    vec4 sum = vec4(0.0);
    float wsum = 0.0;
    for (int x = -4; x <= 4; x++) {
        for (int y = -4; y <= 4; y++) {
            vec2 o = vec2(float(x), float(y));
            float w = exp(-dot(o, o) / 8.0);
            sum += w * IMG_NORM_PIXEL(inputImage, isf_FragNormCoord + o * px);
            wsum += w;
        }
    }
    gl_FragColor = sum / wsum;
}
