/*{
  "DESCRIPTION": "Four-corner perspective warp: drag each corner of the picture.",
  "CATEGORIES": ["Geometry"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "top_left", "TYPE": "point2D", "MIN": [-0.5, -0.5], "MAX": [1.5, 1.5], "DEFAULT": [0.0, 1.0] },
    { "NAME": "top_right", "TYPE": "point2D", "MIN": [-0.5, -0.5], "MAX": [1.5, 1.5], "DEFAULT": [1.0, 1.0] },
    { "NAME": "bottom_left", "TYPE": "point2D", "MIN": [-0.5, -0.5], "MAX": [1.5, 1.5], "DEFAULT": [0.0, 0.0] },
    { "NAME": "bottom_right", "TYPE": "point2D", "MIN": [-0.5, -0.5], "MAX": [1.5, 1.5], "DEFAULT": [1.0, 0.0] }
  ]
}*/
void main() {
    // Projective map from the unit square to the quad (corners in output uv), inverted
    // with the adjugate to find the source point of each output pixel.
    vec2 p0 = bottom_left;
    vec2 p1 = bottom_right;
    vec2 p2 = top_right;
    vec2 p3 = top_left;
    vec2 d1 = p1 - p2;
    vec2 d2 = p3 - p2;
    vec2 d3 = p0 - p1 + p2 - p3;
    float g = 0.0;
    float h = 0.0;
    if (abs(d3.x) > 1e-6 || abs(d3.y) > 1e-6) {
        float den = d1.x * d2.y - d1.y * d2.x;
        if (abs(den) < 1e-9) { gl_FragColor = vec4(0.0); return; }
        g = (d3.x * d2.y - d3.y * d2.x) / den;
        h = (d1.x * d3.y - d1.y * d3.x) / den;
    }
    vec3 c0 = vec3(p1.x - p0.x + g * p1.x, p1.y - p0.y + g * p1.y, g);
    vec3 c1 = vec3(p3.x - p0.x + h * p3.x, p3.y - p0.y + h * p3.y, h);
    vec3 c2 = vec3(p0.x, p0.y, 1.0);
    vec3 pt = vec3(isf_FragNormCoord, 1.0);
    vec3 r = vec3(dot(cross(c1, c2), pt), dot(cross(c2, c0), pt), dot(cross(c0, c1), pt));
    if (abs(r.z) < 1e-9) { gl_FragColor = vec4(0.0); return; }
    vec2 src = r.xy / r.z;
    bool inside = src.x >= -0.0005 && src.x <= 1.0005 && src.y >= -0.0005 && src.y <= 1.0005;
    gl_FragColor = inside ? IMG_NORM_PIXEL(inputImage, clamp(src, 0.0, 1.0)) : vec4(0.0);
}
