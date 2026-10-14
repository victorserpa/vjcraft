/*{
  "DESCRIPTION": "Warms or cools the image and shifts it toward green or magenta.",
  "CATEGORIES": ["Color"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "warmth", "TYPE": "float", "MIN": -1.0, "MAX": 1.0, "DEFAULT": 0.3 },
    { "NAME": "tint", "TYPE": "float", "MIN": -1.0, "MAX": 1.0, "DEFAULT": 0.0 }
  ]
}*/
void main() {
    vec4 c = IMG_THIS_PIXEL(inputImage);
    vec3 gain = vec3(1.0 + 0.4 * warmth, 1.0 - 0.3 * tint, 1.0 - 0.4 * warmth);
    gl_FragColor = vec4(clamp(c.rgb * gain, 0.0, 1.0), c.a);
}
