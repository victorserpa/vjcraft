/*{
  "DESCRIPTION": "Flashes the picture on and off at a fixed rate.",
  "CATEGORIES": ["Time"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "rate", "TYPE": "float", "MIN": 0.1, "MAX": 30.0, "DEFAULT": 4.0 },
    { "NAME": "duty", "TYPE": "float", "MIN": 0.05, "MAX": 0.95, "DEFAULT": 0.5 },
    { "NAME": "floor_level", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.0 },
    { "NAME": "flash_white", "TYPE": "bool", "DEFAULT": false }
  ]
}*/
void main() {
    vec4 c = IMG_THIS_PIXEL(inputImage);
    float on = step(fract(TIME * rate), duty);
    vec3 lit = flash_white ? vec3(1.0) : c.rgb;
    vec3 dark = c.rgb * floor_level;
    gl_FragColor = vec4(mix(dark, lit, on), c.a);
}
