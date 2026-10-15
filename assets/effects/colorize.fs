/*{
  "DESCRIPTION": "Maps luminance between two colours, keeping the image brightness.",
  "CATEGORIES": ["Color"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "shade", "TYPE": "color", "DEFAULT": [0.1, 0.0, 0.3, 1.0] },
    { "NAME": "light", "TYPE": "color", "DEFAULT": [1.0, 0.6, 0.2, 1.0] },
    { "NAME": "amount", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 1.0 }
  ]
}*/
void main() {
    vec4 c = IMG_THIS_PIXEL(inputImage);
    float l = dot(c.rgb, vec3(0.299, 0.587, 0.114));
    gl_FragColor = vec4(mix(c.rgb, mix(shade.rgb, light.rgb, l), amount), c.a);
}
