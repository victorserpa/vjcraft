/*{
  "DESCRIPTION": "Reduces each channel to a few steps.",
  "CATEGORIES": ["Color"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "steps", "TYPE": "float", "MIN": 2.0, "MAX": 32.0, "DEFAULT": 5.0 }
  ]
}*/
void main() {
    vec4 c = IMG_THIS_PIXEL(inputImage);
    float n = max(floor(steps), 2.0) - 1.0;
    gl_FragColor = vec4(floor(c.rgb * n + 0.5) / n, c.a);
}
