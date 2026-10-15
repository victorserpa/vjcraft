/*{
  "DESCRIPTION": "Perspective floor grid rushing toward a horizon, retro synthwave style.",
  "CATEGORIES": ["Generator"],
  "INPUTS": [
    { "NAME": "lineColor", "TYPE": "color", "DEFAULT": [1.0, 0.2, 0.8, 1.0] },
    { "NAME": "floorColor", "TYPE": "color", "DEFAULT": [0.05, 0.0, 0.12, 1.0] },
    { "NAME": "skyTop", "TYPE": "color", "DEFAULT": [0.02, 0.0, 0.1, 1.0] },
    { "NAME": "skyHorizon", "TYPE": "color", "DEFAULT": [0.9, 0.2, 0.5, 1.0] },
    { "NAME": "horizon", "TYPE": "float", "MIN": 0.2, "MAX": 0.8, "DEFAULT": 0.55 },
    { "NAME": "cells", "TYPE": "float", "MIN": 1.0, "MAX": 20.0, "DEFAULT": 6.0 },
    { "NAME": "speed", "TYPE": "float", "MIN": -4.0, "MAX": 4.0, "DEFAULT": 1.0 },
    { "NAME": "lineWidth", "TYPE": "float", "MIN": 0.01, "MAX": 0.3, "DEFAULT": 0.05 }
  ]
}*/
void main() {
    float aspect = RENDERSIZE.x / RENDERSIZE.y;
    vec2 uv = isf_FragNormCoord;
    vec3 col;
    if (uv.y >= horizon) {
        float t = (uv.y - horizon) / (1.0 - horizon);
        col = mix(skyHorizon.rgb, skyTop.rgb, pow(t, 0.6));
    } else {
        float depth = (horizon - uv.y);
        float z = 1.0 / max(depth, 1e-3);
        float x = (uv.x - 0.5) * aspect * z;
        vec2 g = vec2(x, z - TIME * speed) * cells / 6.0;
        vec2 f = abs(fract(g - 0.5) - 0.5);
        vec2 w = fwidth(g) + 1e-4;
        vec2 l = 1.0 - smoothstep(vec2(lineWidth), vec2(lineWidth) + w * 1.5, f);
        float m = max(l.x, l.y);
        // Fade the lines into the horizon haze.
        float fog = smoothstep(0.0, 0.12, depth);
        col = mix(floorColor.rgb, lineColor.rgb, m * fog);
        col = mix(col, skyHorizon.rgb, (1.0 - fog) * 0.8);
    }
    gl_FragColor = vec4(col, 1.0);
}
