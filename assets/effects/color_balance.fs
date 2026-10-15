/*{
  "DESCRIPTION": "Tints shadows, midtones and highlights separately.",
  "CATEGORIES": ["Color"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "shadows", "TYPE": "color", "DEFAULT": [0.5, 0.5, 0.5, 1.0] },
    { "NAME": "midtones", "TYPE": "color", "DEFAULT": [0.5, 0.5, 0.5, 1.0] },
    { "NAME": "highlights", "TYPE": "color", "DEFAULT": [0.5, 0.5, 0.5, 1.0] },
    { "NAME": "strength", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.5 }
  ]
}*/
void main() {
    vec4 c = IMG_THIS_PIXEL(inputImage);
    float l = dot(c.rgb, vec3(0.299, 0.587, 0.114));
    float ws = 1.0 - smoothstep(0.0, 0.5, l);
    float wh = smoothstep(0.5, 1.0, l);
    float wm = 1.0 - ws - wh;
    vec3 shift = (shadows.rgb - 0.5) * ws + (midtones.rgb - 0.5) * wm + (highlights.rgb - 0.5) * wh;
    gl_FragColor = vec4(clamp(c.rgb + shift * strength * 2.0, 0.0, 1.0), c.a);
}
