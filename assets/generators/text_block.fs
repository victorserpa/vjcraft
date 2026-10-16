/*{
  "DESCRIPTION": "Multi-line text block: '|' starts a new line. Left, centre or right alignment, line spacing and rotation.",
  "CATEGORIES": ["Generator"],
  "INPUTS": [
    { "NAME": "message", "TYPE": "text", "DEFAULT": "VJ|CRAFT" },
    { "NAME": "background", "TYPE": "color", "DEFAULT": [0.0, 0.0, 0.0, 1.0] },
    { "NAME": "ink", "TYPE": "color", "DEFAULT": [1.0, 1.0, 1.0, 1.0] },
    { "NAME": "size", "TYPE": "float", "MIN": 0.05, "MAX": 0.6, "DEFAULT": 0.25 },
    { "NAME": "align", "TYPE": "long", "VALUES": [0, 1, 2], "LABELS": ["Left", "Centre", "Right"], "DEFAULT": 1 },
    { "NAME": "leading", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.2 },
    { "NAME": "rotation", "TYPE": "float", "MIN": -180.0, "MAX": 180.0, "DEFAULT": 0.0 }
  ]
}*/
const int FONT[128] = int[128](0, 0, 1556480, 0, 0, 0, 343956, 2687, 0, 0, 137667, 12518, 1402038, 10274, 49664, 0, 560640, 65, 565376, 28, 1019434, 5404, 1016840, 1032, 796672, 0, 132104, 1032, 1585152, 0, 133216, 388, 1206462, 8005, 2089216, 64, 1339586, 9033, 1205441, 6985, 297496, 2175, 1139367, 7365, 1205564, 6217, 161921, 389, 1205430, 6985, 1205382, 3881, 891648, 0, 0, 0, 559624, 65, 330260, 2580, 565376, 1044, 1327234, 777, 1532094, 3925, 148734, 16137, 1205503, 6985, 1073342, 4417, 1073407, 8001, 1205503, 8393, 148735, 137, 1204414, 15689, 132223, 16264, 2089088, 65, 1073184, 191, 328831, 8354, 1056895, 8256, 196991, 16258, 65919, 16264, 1073342, 8001, 148735, 777, 1335486, 12065, 410879, 9001, 1205446, 6345, 2080897, 129, 1056831, 8128, 1052703, 4000, 397439, 16288, 133731, 12692, 1966595, 388, 1206497, 8645, 0, 0, 0, 0, 0, 0, 0, 0, 1056832, 8256);

// Raw character code at index i of the message (4 per uniform slot); lower case folds up.
int raw_at(int i) {
    if (i < 0 || i >= 24) { return 32; }
    float v = isf_u.p[message + i / 4][i - (i / 4) * 4];
    int c = int(v + 0.5);
    if (c >= 97 && c <= 122) { c -= 32; }
    return c;
}

bool glyph_on(int c, int cx, int cy) {
    if (c < 32 || c > 95) { return false; }
    int g = (c - 32) * 2;
    int bits;
    int col = cx;
    if (cx < 3) { bits = FONT[g]; } else { bits = FONT[g + 1]; col = cx - 3; }
    int shift = col * 7 + cy;
    return ((bits >> shift) & 1) == 1;
}

// Characters on the line that starts at message index `start` (a '|' ends a line).
int line_len(int start) {
    int n = 0;
    for (int i = 0; i < 24; i++) {
        if (i < start) { continue; }
        int c = raw_at(i);
        if (c == 124) { break; }
        n += 1;
    }
    return n;
}

int widest() {
    int best = 0;
    int cur = 0;
    for (int i = 0; i < 24; i++) {
        if (raw_at(i) == 124) { cur = 0; } else { cur += 1; best = max(best, cur); }
    }
    return best;
}

int line_count() {
    int n = 1;
    for (int i = 0; i < 24; i++) {
        if (raw_at(i) == 124) { n += 1; }
    }
    return n;
}

void main() {
    float aspect = RENDERSIZE.x / RENDERSIZE.y;
    vec2 p = (isf_FragNormCoord - 0.5) * vec2(aspect, 1.0);
    float ang = radians(rotation);
    float cs = cos(ang);
    float sn = sin(ang);
    // Content turns clockwise by `rotation`: look it up at the point turned the other way.
    vec2 q = vec2(cs * p.x - sn * p.y, sn * p.x + cs * p.y);
    float cell = size / 8.0;
    float pitch = size * (1.0 + leading);
    int lines = line_count();
    float top = (float(lines) * pitch - size * leading) * 0.5;
    int k = int(floor((top - q.y) / pitch));
    vec4 col = background;
    float yy = top - float(k) * pitch - q.y;
    if (k >= 0 && k < lines && yy >= 0.0 && yy < size) {
        // Find the first character of line k.
        int start = 0;
        int seen = 0;
        for (int i = 0; i < 24; i++) {
            if (seen >= k) { break; }
            if (raw_at(i) == 124) { seen += 1; }
            start = i + 1;
        }
        int len = line_len(start);
        float box = float(widest()) * 6.0 * cell;
        float lw = float(len) * 6.0 * cell;
        float x0 = -box * 0.5 + (box - lw) * float(align) * 0.5;
        float gx = (q.x - x0) / cell;
        if (gx >= 0.0) {
            int px = int(floor(gx));
            int ci = px / 6;
            int cx = px - ci * 6;
            int py = int(floor(yy / cell));
            if (ci < len && cx < 5 && py >= 0 && py < 7) {
                if (glyph_on(raw_at(start + ci), cx, py)) { col = ink; }
            }
        }
    }
    gl_FragColor = col;
}
