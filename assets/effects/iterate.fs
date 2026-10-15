/*{
  "DESCRIPTION": "Folds, rotates and zooms the picture over itself several times for a recursive kaleidoscope.",
  "CATEGORIES": ["Geometry"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "iterations", "TYPE": "float", "MIN": 1.0, "MAX": 8.0, "DEFAULT": 4.0 },
    { "NAME": "zoom", "TYPE": "float", "MIN": 1.05, "MAX": 2.0, "DEFAULT": 1.35 },
    { "NAME": "twist", "TYPE": "float", "MIN": -1.0, "MAX": 1.0, "DEFAULT": 0.25 },
    { "NAME": "drift", "TYPE": "float", "MIN": -1.0, "MAX": 1.0, "DEFAULT": 0.1 }
  ]
}*/
void main() {
    float aspect = RENDERSIZE.x / RENDERSIZE.y;
    vec2 p = (isf_FragNormCoord - 0.5) * vec2(aspect, 1.0);
    float a = twist + drift * TIME;
    float cs = cos(a);
    float sn = sin(a);
    for (int i = 0; i < 8; i++) {
        if (float(i) >= iterations) {
            break;
        }
        p = abs(p);
        p = vec2(cs * p.x - sn * p.y, sn * p.x + cs * p.y) * zoom - vec2(0.12, 0.05);
    }
    p.x /= aspect;
    vec2 uv = abs(fract(p * 0.5 + 0.5 - 0.25) * 2.0 - 1.0);
    gl_FragColor = IMG_NORM_PIXEL(inputImage, uv);
}
