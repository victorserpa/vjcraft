/*{
  "DESCRIPTION": "Inverts the channels that are brighter than a threshold.",
  "CATEGORIES": ["Color"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "level", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.5 }
  ]
}*/
void main() {
    vec4 c = IMG_THIS_PIXEL(inputImage);
    vec3 over = step(vec3(level), c.rgb);
    gl_FragColor = vec4(mix(c.rgb, 1.0 - c.rgb, over), c.a);
}
