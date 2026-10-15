/*{
  "DESCRIPTION": "Adds a constant colour to the picture, or subtracts it with a negative amount.",
  "CATEGORIES": ["Color"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "tint", "TYPE": "color", "DEFAULT": [0.2, 0.1, 0.4, 1.0] },
    { "NAME": "amount", "TYPE": "float", "MIN": -1.0, "MAX": 1.0, "DEFAULT": 0.5 }
  ]
}*/
void main() {
    vec4 c = IMG_THIS_PIXEL(inputImage);
    gl_FragColor = vec4(clamp(c.rgb + tint.rgb * amount, 0.0, 1.0), c.a);
}
