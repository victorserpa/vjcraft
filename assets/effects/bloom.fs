/*{
  "DESCRIPTION": "Bloom: bright areas are blurred and added back (two passes).",
  "CATEGORIES": ["Blur"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "threshold", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.6 },
    { "NAME": "radius", "TYPE": "float", "MIN": 1.0, "MAX": 48.0, "DEFAULT": 16.0 },
    { "NAME": "intensity", "TYPE": "float", "MIN": 0.0, "MAX": 3.0, "DEFAULT": 1.0 }
  ],
  "PASSES": [
    { "TARGET": "bright" },
    {}
  ]
}*/
vec3 bright_at(vec2 uv) {
    vec3 c = IMG_NORM_PIXEL(inputImage, uv).rgb;
    float l = dot(c, vec3(0.299, 0.587, 0.114));
    return c * smoothstep(threshold, min(threshold + 0.2, 1.2), l);
}
void main() {
    vec2 uv = isf_FragNormCoord;
    vec2 step = vec2(1.0, 1.0) / RENDERSIZE * radius / 3.0;
    if (PASSINDEX == 0) {
        // Bright extraction, blurred horizontally.
        vec3 sum = vec3(0.0);
        float wsum = 0.0;
        for (int i = -4; i <= 4; i++) {
            float w = exp(-float(i * i) / 10.0);
            sum += bright_at(uv + vec2(float(i) * step.x, 0.0)) * w;
            wsum += w;
        }
        gl_FragColor = vec4(sum / wsum, 1.0);
    } else {
        vec3 sum = vec3(0.0);
        float wsum = 0.0;
        for (int i = -4; i <= 4; i++) {
            float w = exp(-float(i * i) / 10.0);
            sum += IMG_NORM_PIXEL(bright, uv + vec2(0.0, float(i) * step.y)).rgb * w;
            wsum += w;
        }
        vec4 c = IMG_THIS_PIXEL(inputImage);
        gl_FragColor = vec4(clamp(c.rgb + sum / wsum * intensity, 0.0, 1.0), c.a);
    }
}
