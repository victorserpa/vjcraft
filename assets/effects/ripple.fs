/*{
  "DESCRIPTION": "Concentric waves that travel outward from a centre point.",
  "CATEGORIES": ["Distort"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "amplitude", "TYPE": "float", "MIN": 0.0, "MAX": 0.1, "DEFAULT": 0.015 },
    { "NAME": "frequency", "TYPE": "float", "MIN": 1.0, "MAX": 80.0, "DEFAULT": 24.0 },
    { "NAME": "speed", "TYPE": "float", "MIN": 0.0, "MAX": 10.0, "DEFAULT": 3.0 },
    { "NAME": "center", "TYPE": "point2D", "MIN": [0.0, 0.0], "MAX": [1.0, 1.0], "DEFAULT": [0.5, 0.5] }
  ]
}*/
void main() {
    float aspect = RENDERSIZE.x / RENDERSIZE.y;
    vec2 p = (isf_FragNormCoord - center) * vec2(aspect, 1.0);
    float d = length(p);
    vec2 dir = p / max(d, 0.0001);
    float w = amplitude * sin(d * frequency - TIME * speed) / (1.0 + d * 2.0);
    gl_FragColor = IMG_NORM_PIXEL(inputImage, isf_FragNormCoord + dir * w / vec2(aspect, 1.0));
}
