/*{
  "DESCRIPTION": "Crops the picture from the left, right, top and bottom; outside is transparent.",
  "CATEGORIES": ["Geometry"],
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "left", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.0 },
    { "NAME": "right", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.0 },
    { "NAME": "top", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.0 },
    { "NAME": "bottom", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.0 },
    { "NAME": "feather", "TYPE": "float", "MIN": 0.0, "MAX": 0.5, "DEFAULT": 0.0 }
  ]
}*/
void main() {
    vec2 uv = isf_FragNormCoord;
    // Distance inside each edge (ISF uv has its origin at the bottom-left).
    float dl = uv.x - left;
    float dr = (1.0 - right) - uv.x;
    float db = uv.y - bottom;
    float dt = (1.0 - top) - uv.y;
    float d = min(min(dl, dr), min(db, dt));
    float k = feather > 0.0 ? clamp(d / feather, 0.0, 1.0) : step(0.0, d);
    // Strictly outside the box is always cut, even with a feather.
    k = d < 0.0 ? 0.0 : k;
    vec4 c = IMG_THIS_PIXEL(inputImage);
    // Straight colour: the feather fades coverage only (scaling rgb too darkens the edge).
    gl_FragColor = vec4(c.rgb, c.a * k);
}
