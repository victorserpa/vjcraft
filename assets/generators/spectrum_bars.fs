/*{
  "DESCRIPTION": "Spectrum analyser bars fed by the master audio analysis. With no audio the levels are synthetic and move with TIME; set bpm to pulse the bass.",
  "CATEGORIES": ["Generator"],
  "INPUTS": [
    { "NAME": "audioSpec", "TYPE": "audioFFT", "MAX": 64 },
    { "NAME": "colorLow", "TYPE": "color", "DEFAULT": [0.1, 0.9, 0.4, 1.0] },
    { "NAME": "colorHigh", "TYPE": "color", "DEFAULT": [1.0, 0.2, 0.3, 1.0] },
    { "NAME": "background", "TYPE": "color", "DEFAULT": [0.02, 0.02, 0.06, 1.0] },
    { "NAME": "bars", "TYPE": "float", "MIN": 4.0, "MAX": 128.0, "DEFAULT": 32.0 },
    { "NAME": "gap", "TYPE": "float", "MIN": 0.0, "MAX": 0.8, "DEFAULT": 0.2 },
    { "NAME": "bpm", "TYPE": "float", "MIN": 40.0, "MAX": 220.0, "DEFAULT": 120.0 },
    { "NAME": "sensitivity", "TYPE": "float", "MIN": 0.2, "MAX": 1.5, "DEFAULT": 0.9 }
  ]
}*/
float level_(float i, float n) {
    float x = i / n;
    // Alpha of the texture is 1 only while audio is playing.
    if (IMG_NORM_PIXEL(audioSpec, vec2(0.5, 0.5)).a > 0.5) {
        float v = IMG_NORM_PIXEL(audioSpec, vec2((i + 0.5) / n, 0.5)).r;
        return clamp(v * sensitivity * 1.6, 0.02, 1.0);
    }
    float beat = TIME * bpm / 60.0;
    float kick = exp(-fract(beat) * 5.0);
    float low = (1.0 - x) * (0.35 + 0.65 * kick);
    float mid = 0.5 + 0.5 * sin(TIME * (1.7 + x * 3.0) + i * 1.9);
    float hi = 0.5 + 0.5 * sin(TIME * (4.0 + x * 5.0) - i * 0.7);
    float v = low * 0.6 + mid * 0.35 * (1.0 - 0.5 * x) + hi * 0.25 * x;
    return clamp(v * sensitivity * (1.0 - 0.45 * x), 0.02, 1.0);
}
void main() {
    vec2 uv = isf_FragNormCoord;
    float n = floor(bars);
    float i = floor(uv.x * n);
    float fx = fract(uv.x * n);
    float h = level_(i, n);
    float inbar = step(gap * 0.5, fx) * step(fx, 1.0 - gap * 0.5);
    float body = step(uv.y, h) * inbar;
    float cap = step(abs(uv.y - min(h + 0.03, 1.0)), 0.008) * inbar;
    vec4 col = mix(colorLow, colorHigh, uv.y);
    vec4 outc = mix(background, col, body);
    outc = mix(outc, vec4(1.0), cap * 0.8);
    gl_FragColor = vec4(outc.rgb, 1.0);
}
