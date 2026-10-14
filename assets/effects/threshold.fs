/*{
  "DESCRIPTION": "Hard or soft black and white split by brightness, with custom colours.",
  "CATEGORIES": ["Color"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "level", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.5 },
    { "NAME": "softness", "TYPE": "float", "MIN": 0.0, "MAX": 0.5, "DEFAULT": 0.02 },
    { "NAME": "dark", "TYPE": "color", "DEFAULT": [0.0, 0.0, 0.0, 1.0] },
    { "NAME": "light", "TYPE": "color", "DEFAULT": [1.0, 1.0, 1.0, 1.0] }
  ]
}*/
void main() {
    vec4 c = IMG_THIS_PIXEL(inputImage);
    float l = dot(c.rgb, vec3(0.2126, 0.7152, 0.0722));
    float k = smoothstep(level - softness - 0.0005, level + softness + 0.0005, l);
    gl_FragColor = vec4(mix(dark.rgb, light.rgb, k), c.a);
}
