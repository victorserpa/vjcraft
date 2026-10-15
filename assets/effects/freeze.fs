/*{
  "DESCRIPTION": "Stop motion: grabs a new frame at a fixed rate, or holds the current one.",
  "CATEGORIES": ["Time"],
  "PERSISTENT": true,
  "TICK_INPUT": "fps",
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "fps", "TYPE": "float", "MIN": 0.5, "MAX": 60.0, "DEFAULT": 4.0 },
    { "NAME": "hold", "TYPE": "bool", "DEFAULT": false }
  ]
}*/
void main() {
    // NEW_TICK comes from the runtime, counted per instance in Rust:
    // 2 = first draw (nothing kept yet), 1 = a new tick, 0 = same tick.
    bool grab = NEW_TICK > 1.5 || (NEW_TICK > 0.5 && !hold);
    // The kept frame is the input as it was, alpha included.
    gl_FragColor = grab ? IMG_THIS_PIXEL(inputImage) : PREV_THIS_PIXEL;
}
