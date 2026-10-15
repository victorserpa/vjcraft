/*{
  "DESCRIPTION": "Anamorphic lens flare with ghosts along the axis through the centre.",
  "CATEGORIES": ["Generator"],
  "INPUTS": [
    { "NAME": "background", "TYPE": "color", "DEFAULT": [0.0, 0.0, 0.03, 1.0] },
    { "NAME": "tint", "TYPE": "color", "DEFAULT": [1.0, 0.75, 0.45, 1.0] },
    { "NAME": "position", "TYPE": "point2D", "DEFAULT": [0.35, 0.65] },
    { "NAME": "intensity", "TYPE": "float", "MIN": 0.0, "MAX": 3.0, "DEFAULT": 1.0 },
    { "NAME": "streak", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.6 },
    { "NAME": "drift", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.1 }
  ]
}*/
void main() {
    float aspect = RENDERSIZE.x / RENDERSIZE.y;
    vec2 asp = vec2(aspect, 1.0);
    vec2 src = position + drift * vec2(sin(TIME * 0.5), cos(TIME * 0.37)) * 0.3;
    vec2 d = (isf_FragNormCoord - src) * asp;
    float r = length(d);
    vec3 c = tint.rgb * (0.02 / (r * r + 0.002)) * 0.1;
    c += tint.rgb * streak * 0.02 / (abs(d.y) * 12.0 + 0.02) * exp(-abs(d.x) * 2.5);
    vec2 axis = (vec2(0.5) - src) * asp;
    for (int i = 1; i <= 4; i++) {
        float fi = float(i);
        vec2 gp = src * asp + axis * (0.5 + 0.45 * fi);
        float gr = 0.04 + 0.03 * fi;
        float ring = smoothstep(gr, gr * 0.6, length(isf_FragNormCoord * asp - gp));
        vec3 gc = mix(tint.rgb, vec3(0.4, 0.7, 1.0), fi * 0.25);
        c += gc * ring * 0.18 / fi;
    }
    gl_FragColor = vec4(background.rgb + c * intensity, 1.0);
}
