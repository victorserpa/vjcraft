/*{
  "DESCRIPTION": "Dark horizontal lines over the picture.",
  "CATEGORIES": ["Stylize"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "lines", "TYPE": "float", "MIN": 20.0, "MAX": 1080.0, "DEFAULT": 240.0 },
    { "NAME": "darkness", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.5 },
    { "NAME": "roll", "TYPE": "float", "MIN": -2.0, "MAX": 2.0, "DEFAULT": 0.0 }
  ]
}*/
void main() {
    vec4 c = IMG_THIS_PIXEL(inputImage);
    float s = 0.5 + 0.5 * sin((isf_FragNormCoord.y + TIME * roll) * lines * 6.2831853);
    gl_FragColor = vec4(c.rgb * (1.0 - darkness * s), c.a);
}
