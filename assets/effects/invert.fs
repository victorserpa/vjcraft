/*{
  "DESCRIPTION": "Inverts the colors.",
  "CATEGORIES": ["Color"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "amount", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 1.0 }
  ]
}*/
void main() {
    vec4 c = IMG_THIS_PIXEL(inputImage);
    gl_FragColor = vec4(mix(c.rgb, vec3(1.0) - c.rgb, amount), c.a);
}
