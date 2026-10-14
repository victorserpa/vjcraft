/*{
  "DESCRIPTION": "Camera-shake: jumps the picture by a random offset several times per second.",
  "CATEGORIES": ["Time"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "amount", "TYPE": "float", "MIN": 0.0, "MAX": 0.2, "DEFAULT": 0.02 },
    { "NAME": "rate", "TYPE": "float", "MIN": 1.0, "MAX": 60.0, "DEFAULT": 12.0 }
  ]
}*/
float hash(vec2 p) {
    return fract(sin(dot(p, vec2(127.1, 311.7))) * 43758.5453);
}
void main() {
    float t = floor(TIME * rate);
    vec2 o = vec2(hash(vec2(t, 1.0)), hash(vec2(t, 2.0))) - 0.5;
    gl_FragColor = IMG_NORM_PIXEL(inputImage, isf_FragNormCoord + o * 2.0 * amount);
}
