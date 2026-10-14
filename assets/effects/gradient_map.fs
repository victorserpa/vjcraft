/*{
  "DESCRIPTION": "Replaces brightness with a three-stop colour ramp.",
  "CATEGORIES": ["Color"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "low", "TYPE": "color", "DEFAULT": [0.02, 0.0, 0.15, 1.0] },
    { "NAME": "mid", "TYPE": "color", "DEFAULT": [0.9, 0.2, 0.4, 1.0] },
    { "NAME": "high", "TYPE": "color", "DEFAULT": [1.0, 0.95, 0.6, 1.0] },
    { "NAME": "midpoint", "TYPE": "float", "MIN": 0.05, "MAX": 0.95, "DEFAULT": 0.5 }
  ]
}*/
void main() {
    vec4 c = IMG_THIS_PIXEL(inputImage);
    float l = clamp(dot(c.rgb, vec3(0.2126, 0.7152, 0.0722)), 0.0, 1.0);
    vec3 lo = mix(low.rgb, mid.rgb, smoothstep(0.0, midpoint, l));
    vec3 col = mix(lo, high.rgb, smoothstep(midpoint, 1.0, l));
    gl_FragColor = vec4(col, c.a);
}
