/*{
  "DESCRIPTION": "Tints shadows and highlights with separate colours.",
  "CATEGORIES": ["Color"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "shadows", "TYPE": "color", "DEFAULT": [0.1, 0.35, 0.6, 1.0] },
    { "NAME": "highlights", "TYPE": "color", "DEFAULT": [1.0, 0.65, 0.35, 1.0] },
    { "NAME": "balance", "TYPE": "float", "MIN": 0.1, "MAX": 0.9, "DEFAULT": 0.5 },
    { "NAME": "strength", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.5 }
  ]
}*/
void main() {
    vec4 c = IMG_THIS_PIXEL(inputImage);
    float l = dot(c.rgb, vec3(0.2126, 0.7152, 0.0722));
    float k = smoothstep(balance - 0.25, balance + 0.25, l);
    vec3 tone = mix(shadows.rgb, highlights.rgb, k);
    vec3 soft = c.rgb * (tone * 2.0);
    gl_FragColor = vec4(clamp(mix(c.rgb, soft, strength), 0.0, 1.0), c.a);
}
