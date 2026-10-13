/*{
  "DESCRIPTION": "Brightness and contrast.",
  "CATEGORIES": ["Color"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "brightness", "TYPE": "float", "MIN": -1.0, "MAX": 1.0, "DEFAULT": 0.0 },
    { "NAME": "contrast", "TYPE": "float", "MIN": 0.0, "MAX": 3.0, "DEFAULT": 1.0 }
  ]
}*/
void main() {
    vec4 c = IMG_THIS_PIXEL(inputImage);
    vec3 rgb = (c.rgb - vec3(0.5)) * contrast + vec3(0.5) + vec3(brightness);
    gl_FragColor = vec4(clamp(rgb, 0.0, 1.0), c.a);
}
