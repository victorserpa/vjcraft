/*{
  "DESCRIPTION": "Scrolling or static text banner (up to 16 characters, A-Z 0-9 and basic punctuation).",
  "CATEGORIES": ["Generator"],
  "INPUTS": [
    { "NAME": "message", "TYPE": "text", "DEFAULT": "VJCRAFT", "MAXLEN": 16 },
    { "NAME": "background", "TYPE": "color", "DEFAULT": [0.0, 0.0, 0.0, 1.0] },
    { "NAME": "ink", "TYPE": "color", "DEFAULT": [1.0, 1.0, 1.0, 1.0] },
    { "NAME": "size", "TYPE": "float", "MIN": 0.1, "MAX": 1.0, "DEFAULT": 0.6 },
    { "NAME": "scroll", "TYPE": "float", "MIN": 0.0, "MAX": 2.0, "DEFAULT": 0.0 }
  ]
}*/
const int FONT[128] = int[128](0, 0, 1556480, 0, 0, 0, 343956, 2687, 0, 0, 137667, 12518, 1402038, 10274, 49664, 0, 560640, 65, 565376, 28, 1019434, 5404, 1016840, 1032, 796672, 0, 132104, 1032, 1585152, 0, 133216, 388, 1206462, 8005, 2089216, 64, 1339586, 9033, 1205441, 6985, 297496, 2175, 1139367, 7365, 1205564, 6217, 161921, 389, 1205430, 6985, 1205382, 3881, 891648, 0, 0, 0, 559624, 65, 330260, 2580, 565376, 1044, 1327234, 777, 1532094, 3925, 148734, 16137, 1205503, 6985, 1073342, 4417, 1073407, 8001, 1205503, 8393, 148735, 137, 1204414, 15689, 132223, 16264, 2089088, 65, 1073184, 191, 328831, 8354, 1056895, 8256, 196991, 16258, 65919, 16264, 1073342, 8001, 148735, 777, 1335486, 12065, 410879, 9001, 1205446, 6345, 2080897, 129, 1056831, 8128, 1052703, 4000, 397439, 16288, 133731, 12692, 1966595, 388, 1206497, 8645, 0, 0, 0, 0, 0, 0, 0, 0, 1056832, 8256);

// Character code at position i of the message (4 per uniform slot).
int char_at(int i) {
    if (i < 0 || i >= 16) { return 32; }
    float v = isf_u.p[message + i / 4][i - (i / 4) * 4];
    int c = int(v + 0.5);
    if (c >= 97 && c <= 122) { c -= 32; }
    if (c < 32 || c > 95) { c = 32; }
    return c;
}

bool glyph_on(int c, int cx, int cy) {
    int g = (c - 32) * 2;
    int bits;
    int col = cx;
    if (cx < 3) { bits = FONT[g]; } else { bits = FONT[g + 1]; col = cx - 3; }
    int shift = col * 7 + cy;
    return ((bits >> shift) & 1) == 1;
}

void main() {
    vec2 uv = isf_FragNormCoord;
    float aspect = RENDERSIZE.x / RENDERSIZE.y;
    // Cell: 6 columns x 8 rows of font pixels per character; text height = size.
    float cell_h = size / 8.0;
    float cell_w = cell_h / aspect;
    float total_w = 16.0 * 6.0 * cell_w;
    float x = (uv.x - 0.5) + total_w * 0.5 - scroll * TIME * 0.25;
    x = x - floor(x / (total_w + 1.0)) * (total_w + 1.0);
    float y = (uv.y - 0.5) + size * 0.5;
    vec4 col = background;
    if (y >= 0.0 && y < size && x >= 0.0 && x < total_w) {
        int px = int(floor(x / cell_w));
        int py = 7 - int(floor(y / cell_h));
        int ci = px / 6;
        int cx = px - ci * 6;
        if (cx < 5 && py >= 0 && py < 7) {
            if (glyph_on(char_at(ci), cx, py)) { col = ink; }
        }
    }
    gl_FragColor = col;
}
