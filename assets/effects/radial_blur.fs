/*{
  "DESCRIPTION": "Spin blur: smears the image around a centre point.",
  "CATEGORIES": ["Blur"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "amount", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.15 },
    { "NAME": "center", "TYPE": "point2D", "MIN": [0.0, 0.0], "MAX": [1.0, 1.0], "DEFAULT": [0.5, 0.5] }
  ]
}*/
void main() {
    vec2 p = isf_FragNormCoord - center;
    vec4 sum = vec4(0.0);
    for (int i = 0; i < 16; i++) {
        float t = (float(i) / 15.0 - 0.5) * amount;
        float cs = cos(t);
        float sn = sin(t);
        vec2 q = vec2(p.x * cs - p.y * sn, p.x * sn + p.y * cs);
        sum += IMG_NORM_PIXEL(inputImage, center + q);
    }
    gl_FragColor = sum / 16.0;
}
