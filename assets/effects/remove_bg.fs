/*{
  "DESCRIPTION": "Removes the background of a person with an ML matting model (Robust Video Matting). Needs rvm_mobilenetv3_fp32.onnx, see docs/ml.md.",
  "CATEGORIES": ["ML"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "mode", "TYPE": "long", "VALUES": [0, 1, 2], "LABELS": ["Transparent", "Solid Color", "Blur"], "DEFAULT": 0 },
    { "NAME": "threshold", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.5 },
    { "NAME": "feather", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.3 },
    { "NAME": "bg_color", "TYPE": "color", "DEFAULT": [0.0, 0.8, 0.2, 1.0] },
    { "NAME": "blur", "TYPE": "float", "MIN": 0.0, "MAX": 40.0, "DEFAULT": 12.0 }
  ]
}*/
void main() {
    vec4 c = IMG_THIS_PIXEL(inputImage);
    float m = IMG_THIS_PIXEL(vjc_mask).r;
    float f = max(feather * 0.5, 0.0005);
    float a = smoothstep(threshold - f, threshold + f, m);
    if (mode == 0) {
        gl_FragColor = vec4(c.rgb, c.a * a);
    } else if (mode == 1) {
        gl_FragColor = vec4(mix(bg_color.rgb, c.rgb, a), c.a);
    } else {
        // Golden-angle spiral blur of the whole frame, kept only behind the subject.
        vec2 px = vec2(blur) / RENDERSIZE;
        vec3 sum = c.rgb;
        for (int i = 1; i <= 32; i++) {
            float r = sqrt(float(i) / 32.0);
            float t = float(i) * 2.399963;
            sum += IMG_NORM_PIXEL(inputImage, isf_FragNormCoord + vec2(cos(t), sin(t)) * r * px).rgb;
        }
        gl_FragColor = vec4(mix(sum / 33.0, c.rgb, a), c.a);
    }
}
