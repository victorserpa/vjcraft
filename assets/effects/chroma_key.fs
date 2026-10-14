/*{
  "DESCRIPTION": "Makes pixels close to a key colour transparent and suppresses colour spill.",
  "CATEGORIES": ["Keying"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "key", "TYPE": "color", "DEFAULT": [0.0, 1.0, 0.0, 1.0] },
    { "NAME": "tolerance", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.35 },
    { "NAME": "softness", "TYPE": "float", "MIN": 0.0, "MAX": 0.5, "DEFAULT": 0.12 },
    { "NAME": "spill", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.5 }
  ]
}*/
vec2 chroma(vec3 c) {
    return vec2(c.r - c.g * 0.5 - c.b * 0.5, (c.g - c.b) * 0.8660254);
}
void main() {
    vec4 c = IMG_THIS_PIXEL(inputImage);
    float d = length(chroma(c.rgb) - chroma(key.rgb));
    float k = smoothstep(tolerance, tolerance + softness + 0.0005, d);
    float l = dot(c.rgb, vec3(0.2126, 0.7152, 0.0722));
    vec3 clean = mix(c.rgb, vec3(l), spill * (1.0 - k));
    gl_FragColor = vec4(clean, c.a * k);
}
