/*{
  "DESCRIPTION": "Darkens (or tints) the edges.",
  "CATEGORIES": ["Stylize"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "size", "TYPE": "float", "MIN": 0.0, "MAX": 1.5, "DEFAULT": 0.8 },
    { "NAME": "softness", "TYPE": "float", "MIN": 0.01, "MAX": 1.0, "DEFAULT": 0.5 },
    { "NAME": "tint", "TYPE": "color", "DEFAULT": [0.0, 0.0, 0.0, 1.0] }
  ]
}*/
void main() {
    vec4 c = IMG_THIS_PIXEL(inputImage);
    float d = length(isf_FragNormCoord - vec2(0.5)) * 1.4142136;
    float k = 1.0 - smoothstep(size - softness, size, d);
    gl_FragColor = vec4(mix(tint.rgb, c.rgb, k), c.a);
}
