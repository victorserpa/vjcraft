/*{
  "DESCRIPTION": "Shoves random horizontal bands sideways in steps.",
  "CATEGORIES": ["Distort"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "bands", "TYPE": "float", "MIN": 2.0, "MAX": 64.0, "DEFAULT": 16.0 },
    { "NAME": "shift", "TYPE": "float", "MIN": 0.0, "MAX": 0.5, "DEFAULT": 0.08 },
    { "NAME": "rate", "TYPE": "float", "MIN": 0.0, "MAX": 30.0, "DEFAULT": 8.0 },
    { "NAME": "density", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.35 }
  ]
}*/
float hash(vec2 p) {
    return fract(sin(dot(p, vec2(127.1, 311.7))) * 43758.5453);
}
void main() {
    float row = floor(isf_FragNormCoord.y * bands);
    float tick = floor(TIME * rate);
    float on = step(hash(vec2(row, tick)), density);
    float off = (hash(vec2(row + 31.0, tick)) - 0.5) * 2.0 * shift * on;
    gl_FragColor = IMG_NORM_PIXEL(inputImage, isf_FragNormCoord + vec2(off, 0.0));
}
