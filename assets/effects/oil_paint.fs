/*{
  "DESCRIPTION": "Oil painting look (Kuwahara filter, four quadrants).",
  "CATEGORIES": ["Stylize"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "radius", "TYPE": "float", "MIN": 1.0, "MAX": 6.0, "DEFAULT": 3.0 }
  ]
}*/
void main() {
    vec2 uv = isf_FragNormCoord;
    vec2 px = 1.0 / RENDERSIZE;
    int r = int(radius);
    vec3 best = vec3(0.0);
    float bestVar = 1e9;
    for (int q = 0; q < 4; q++) {
        vec2 s = vec2(q == 0 || q == 3 ? -1.0 : 1.0, q < 2 ? -1.0 : 1.0);
        vec3 sum = vec3(0.0);
        float sum2 = 0.0;
        float n = 0.0;
        for (int j = 0; j <= 6; j++) {
            for (int i = 0; i <= 6; i++) {
                if (i <= r && j <= r) {
                    vec3 c = IMG_NORM_PIXEL(inputImage, uv + vec2(float(i), float(j)) * s * px).rgb;
                    float l = dot(c, vec3(0.299, 0.587, 0.114));
                    sum += c;
                    sum2 += l * l;
                    n += 1.0;
                }
            }
        }
        vec3 mean = sum / n;
        float ml = dot(mean, vec3(0.299, 0.587, 0.114));
        float v = sum2 / n - ml * ml;
        if (v < bestVar) {
            bestVar = v;
            best = mean;
        }
    }
    gl_FragColor = vec4(best, IMG_THIS_PIXEL(inputImage).a);
}
