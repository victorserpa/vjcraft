/*{
  "DESCRIPTION": "Motion trails: bright pixels fade out slowly instead of vanishing.",
  "CATEGORIES": ["Feedback"],
  "PERSISTENT": true,
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "decay", "TYPE": "float", "MIN": 0.0, "MAX": 0.98, "DEFAULT": 0.85 }
  ]
}*/
void main() {
    vec4 now = IMG_THIS_PIXEL(inputImage);
    // Subtract one 8-bit step so the trail reaches black instead of stalling on rounding.
    vec4 old = max(PREV_THIS_PIXEL * decay - vec4(1.0 / 255.0), vec4(0.0));
    gl_FragColor = max(now, old);
}
