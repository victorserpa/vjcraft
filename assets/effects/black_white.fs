/*{
  "DESCRIPTION": "Black and white with per-channel weights.",
  "CATEGORIES": ["Color"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "red", "TYPE": "float", "MIN": -1.0, "MAX": 2.0, "DEFAULT": 0.3 },
    { "NAME": "green", "TYPE": "float", "MIN": -1.0, "MAX": 2.0, "DEFAULT": 0.59 },
    { "NAME": "blue", "TYPE": "float", "MIN": -1.0, "MAX": 2.0, "DEFAULT": 0.11 }
  ]
}*/
void main() {
    vec4 c = IMG_THIS_PIXEL(inputImage);
    float g = clamp(dot(c.rgb, vec3(red, green, blue)), 0.0, 1.0);
    gl_FragColor = vec4(vec3(g), c.a);
}
