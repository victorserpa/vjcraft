/*{
  "DESCRIPTION": "S-shaped contrast curve: darks get darker, lights get lighter, mid-tones move around a pivot.",
  "CATEGORIES": ["Color"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "strength", "TYPE": "float", "MIN": -1.0, "MAX": 1.0, "DEFAULT": 0.6 },
    { "NAME": "pivot", "TYPE": "float", "MIN": 0.1, "MAX": 0.9, "DEFAULT": 0.5 }
  ]
}*/
void main() {
    vec4 c = IMG_THIS_PIXEL(inputImage);
    vec3 x = clamp(c.rgb, 0.0, 1.0);
    // Remap so that the pivot sits at 0.5, apply smoothstep as the S, map back.
    vec3 lo = x / (2.0 * pivot);
    vec3 hi = 0.5 + (x - pivot) / (2.0 * (1.0 - pivot));
    vec3 t = clamp(mix(lo, hi, step(vec3(pivot), x)), 0.0, 1.0);
    vec3 s = t * t * (3.0 - 2.0 * t);
    vec3 out_t = mix(t, s, abs(strength));
    if (strength < 0.0) {
        out_t = mix(t, 2.0 * t - s, abs(strength));
    }
    vec3 lo_b = out_t * (2.0 * pivot);
    vec3 hi_b = pivot + (out_t - 0.5) * (2.0 * (1.0 - pivot));
    vec3 r = mix(lo_b, hi_b, step(vec3(0.5), out_t));
    gl_FragColor = vec4(clamp(r, 0.0, 1.0), c.a);
}
