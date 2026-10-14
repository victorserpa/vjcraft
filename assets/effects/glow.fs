/*{
  "DESCRIPTION": "Bloom: blurs the bright parts and adds them back on top.",
  "CATEGORIES": ["Stylize"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "cutoff", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.55 },
    { "NAME": "radius", "TYPE": "float", "MIN": 1.0, "MAX": 60.0, "DEFAULT": 14.0 },
    { "NAME": "intensity", "TYPE": "float", "MIN": 0.0, "MAX": 4.0, "DEFAULT": 1.2 }
  ]
}*/
vec3 bright(vec2 uv) {
    vec3 c = IMG_NORM_PIXEL(inputImage, uv).rgb;
    float l = dot(c, vec3(0.2126, 0.7152, 0.0722));
    return c * smoothstep(cutoff, cutoff + 0.2, l);
}
void main() {
    vec4 c = IMG_THIS_PIXEL(inputImage);
    vec2 px = vec2(1.0) / RENDERSIZE;
    vec3 sum = bright(isf_FragNormCoord);
    float wsum = 1.0;
    for (int ring = 1; ring <= 3; ring++) {
        float rr = radius * float(ring) / 3.0;
        for (int i = 0; i < 8; i++) {
            float a = float(i) * 0.785398 + float(ring) * 0.3;
            vec2 o = vec2(cos(a), sin(a)) * rr * px;
            float w = 1.0 / (1.0 + float(ring));
            sum += w * bright(isf_FragNormCoord + o);
            wsum += w;
        }
    }
    gl_FragColor = vec4(clamp(c.rgb + sum / wsum * intensity, 0.0, 1.0), c.a);
}
