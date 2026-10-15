/*{
  "DESCRIPTION": "Feedback trails built with a persistent buffer: bright pixels fade out slowly.",
  "CATEGORIES": ["Feedback"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "decay", "TYPE": "float", "MIN": 0.0, "MAX": 0.98, "DEFAULT": 0.85 }
  ],
  "PASSES": [
    { "TARGET": "trail", "PERSISTENT": true },
    {}
  ]
}*/
void main() {
    if (PASSINDEX == 0) {
        // `trail` still holds last frame here; the buffer is updated after this pass.
        vec4 now = IMG_THIS_PIXEL(inputImage);
        vec4 old = max(IMG_THIS_PIXEL(trail) * decay - vec4(1.0 / 255.0), vec4(0.0));
        gl_FragColor = max(now, old);
    } else {
        gl_FragColor = IMG_THIS_PIXEL(trail);
    }
}
