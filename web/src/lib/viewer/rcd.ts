// Full-resolution demosaicing on the GPU: RCD (Ratio Corrected Demosaicing, v2.3), the
// same steps as the server's `images/demosaic.rs`, as six WebGL2 passes over a tile's
// sensor samples (from /cfatile: black-subtracted, scaled, white-balanced). The last
// pass colours the result with the camera matrix and the view's tone curve and writes it
// straight into the viewer's tile array (see gl.ts), so a tile costs the server only the
// sample copy and the GPU a millisecond or two.

/** A tile's demosaic input, as served by /photo/{id}/cfatile. */
export interface CfaTile {
  /** Input size (the tile, its apron and a margin on each side). */
  w: number;
  h: number;
  margin: number;
  apron: number;
  /** The tile's own size (before the apron). */
  tw: number;
  th: number;
  /** Colour (0 R, 1 G, 2 B) of the 2x2 positions: top-left, top-right, bottom-left, bottom-right. */
  pattern: [number, number, number, number];
  /** Camera RGB to linear sRGB, row-major. */
  matrix: number[];
  /** Samples (0..1) in 1/65535 steps, row by row. */
  data: Uint16Array;
}

export function parseCfaTile(buf: ArrayBuffer): CfaTile {
  const v = new DataView(buf);
  if (buf.byteLength < 64 || String.fromCharCode(v.getUint8(0), v.getUint8(1), v.getUint8(2), v.getUint8(3)) !== 'HFC1') {
    throw new Error('not a tile');
  }
  const u16 = (o: number) => v.getUint16(o, true);
  const [w, h, margin, apron, tw, th] = [4, 6, 8, 10, 12, 14].map(u16);
  const pattern = [16, 17, 18, 19].map((o) => v.getUint8(o)) as CfaTile['pattern'];
  const matrix = Array.from({ length: 9 }, (_, k) => v.getFloat32(20 + 4 * k, true));
  if (buf.byteLength < 64 + w * h * 2) throw new Error('short tile');
  const data = new Uint16Array(buf, 64, w * h);
  return { w, h, margin, apron, tw, th, pattern, matrix, data };
}

const VS = `#version 300 es
in vec2 p;
void main() { gl_Position = vec4(p, 0.0, 1.0); }`;

const COMMON = `#version 300 es
precision highp float;
precision highp int;
uniform highp usampler2D u_cfa;
uniform ivec4 u_pat;
uniform ivec2 u_size;
out vec4 o;
const float EPS = 1e-5;
const float EPSSQ = 1e-10;
int fc(ivec2 p) {
  int k = (p.y & 1) * 2 + (p.x & 1);
  return k == 0 ? u_pat.x : k == 1 ? u_pat.y : k == 2 ? u_pat.z : u_pat.w;
}
const float UNIT16 = 1.0 / 65535.0;
float c(ivec2 p) { return float(texelFetch(u_cfa, p, 0).r) * UNIT16; }
float c(ivec2 p, int dx, int dy) { return float(texelFetch(u_cfa, p + ivec2(dx, dy), 0).r) * UNIT16; }
float sq(float x) { return x * x; }
bool inner(ivec2 p, int b) { return p.x >= b && p.y >= b && p.x < u_size.x - b && p.y < u_size.y - b; }
float refine(float central, float around) { return abs(0.5 - central) < abs(0.5 - around) ? around : central; }
`;

// Step 1: vertical / horizontal discrimination (high-pass of the colour differences in
// each direction, their local sums as a weight).
const FS_VH = `${COMMON}
float vhpf(ivec2 q) { return sq((c(q, 0, -3) - c(q, 0, -1) - c(q, 0, 1) + c(q, 0, 3)) - 3.0 * (c(q, 0, -2) + c(q, 0, 2)) + 6.0 * c(q)); }
float hhpf(ivec2 q) { return sq((c(q, -3, 0) - c(q, -1, 0) - c(q, 1, 0) + c(q, 3, 0)) - 3.0 * (c(q, -2, 0) + c(q, 2, 0)) + 6.0 * c(q)); }
void main() {
  ivec2 p = ivec2(gl_FragCoord.xy);
  if (!inner(p, 4)) { o = vec4(0.0); return; }
  float v = max(vhpf(p + ivec2(0, -1)) + vhpf(p) + vhpf(p + ivec2(0, 1)), EPSSQ);
  float h = max(hhpf(p + ivec2(-1, 0)) + hhpf(p) + hhpf(p + ivec2(1, 0)), EPSSQ);
  o = vec4(v / (v + h), 0.0, 0.0, 1.0);
}`;

// Step 2: low-pass filter at red and blue samples.
const FS_LPF = `${COMMON}
void main() {
  ivec2 p = ivec2(gl_FragCoord.xy);
  if (!inner(p, 2) || fc(p) == 1) { o = vec4(0.0); return; }
  float v = c(p) + 0.5 * (c(p, 0, -1) + c(p, 0, 1) + c(p, -1, 0) + c(p, 1, 0)) + 0.25 * (c(p, -1, -1) + c(p, 1, -1) + c(p, -1, 1) + c(p, 1, 1));
  o = vec4(v, 0.0, 0.0, 1.0);
}`;

// Step 3: green at red and blue samples, from ratio-corrected cardinal estimates.
const FS_GREEN = `${COMMON}
uniform highp sampler2D u_vh;
uniform highp sampler2D u_lpf;
float vh(ivec2 p, int dx, int dy) { return texelFetch(u_vh, p + ivec2(dx, dy), 0).r; }
float lpf(ivec2 p, int dx, int dy) { return texelFetch(u_lpf, p + ivec2(dx, dy), 0).r; }
void main() {
  ivec2 p = ivec2(gl_FragCoord.xy);
  if (fc(p) == 1) { o = vec4(c(p), 0.0, 0.0, 1.0); return; }
  if (!inner(p, 4)) { o = vec4(0.0); return; }
  float disc = refine(vh(p, 0, 0), 0.25 * (vh(p, -1, -1) + vh(p, 1, -1) + vh(p, -1, 1) + vh(p, 1, 1)));
  float nGrad = EPS + abs(c(p, 0, -1) - c(p, 0, 1)) + abs(c(p) - c(p, 0, -2)) + abs(c(p, 0, -1) - c(p, 0, -3)) + abs(c(p, 0, -2) - c(p, 0, -4));
  float sGrad = EPS + abs(c(p, 0, -1) - c(p, 0, 1)) + abs(c(p) - c(p, 0, 2)) + abs(c(p, 0, 1) - c(p, 0, 3)) + abs(c(p, 0, 2) - c(p, 0, 4));
  float wGrad = EPS + abs(c(p, -1, 0) - c(p, 1, 0)) + abs(c(p) - c(p, -2, 0)) + abs(c(p, -1, 0) - c(p, -3, 0)) + abs(c(p, -2, 0) - c(p, -4, 0));
  float eGrad = EPS + abs(c(p, -1, 0) - c(p, 1, 0)) + abs(c(p) - c(p, 2, 0)) + abs(c(p, 1, 0) - c(p, 3, 0)) + abs(c(p, 2, 0) - c(p, 4, 0));
  float l = lpf(p, 0, 0);
  float nEst = c(p, 0, -1) * (1.0 + (l - lpf(p, 0, -2)) / (EPS + l + lpf(p, 0, -2)));
  float sEst = c(p, 0, 1) * (1.0 + (l - lpf(p, 0, 2)) / (EPS + l + lpf(p, 0, 2)));
  float wEst = c(p, -1, 0) * (1.0 + (l - lpf(p, -2, 0)) / (EPS + l + lpf(p, -2, 0)));
  float eEst = c(p, 1, 0) * (1.0 + (l - lpf(p, 2, 0)) / (EPS + l + lpf(p, 2, 0)));
  float vEst = (sGrad * nEst + nGrad * sEst) / (nGrad + sGrad);
  float hEst = (wGrad * eEst + eGrad * wEst) / (eGrad + wGrad);
  o = vec4(clamp(disc * hEst + (1.0 - disc) * vEst, 0.0, 1.0), 0.0, 0.0, 1.0);
}`;

// Step 4.0/4.1: diagonal (P = NW-SE, Q = NE-SW) discrimination at red and blue samples.
const FS_PQ = `${COMMON}
float phpf(ivec2 q) { return sq((c(q, -3, -3) - c(q, -1, -1) - c(q, 1, 1) + c(q, 3, 3)) - 3.0 * (c(q, -2, -2) + c(q, 2, 2)) + 6.0 * c(q)); }
float qhpf(ivec2 q) { return sq((c(q, 3, -3) - c(q, 1, -1) - c(q, -1, 1) + c(q, -3, 3)) - 3.0 * (c(q, 2, -2) + c(q, -2, 2)) + 6.0 * c(q)); }
void main() {
  ivec2 p = ivec2(gl_FragCoord.xy);
  if (!inner(p, 4) || fc(p) == 1) { o = vec4(0.0); return; }
  float P = max(phpf(p + ivec2(-1, -1)) + phpf(p) + phpf(p + ivec2(1, 1)), EPSSQ);
  float Q = max(qhpf(p + ivec2(1, -1)) + qhpf(p) + qhpf(p + ivec2(-1, 1)), EPSSQ);
  o = vec4(P / (P + Q), 0.0, 0.0, 1.0);
}`;

// Step 4.2: red at blue samples and blue at red ones, from diagonal colour differences.
// Output: (red, blue) at red and blue samples.
const FS_RB = `${COMMON}
uniform highp sampler2D u_g;
uniform highp sampler2D u_pq;
float g(ivec2 p, int dx, int dy) { return texelFetch(u_g, p + ivec2(dx, dy), 0).r; }
float pq(ivec2 p, int dx, int dy) { return texelFetch(u_pq, p + ivec2(dx, dy), 0).r; }
void main() {
  ivec2 p = ivec2(gl_FragCoord.xy);
  int own = fc(p);
  if (own == 1) { o = vec4(0.0); return; }
  float cv = c(p);
  if (!inner(p, 4)) { o = own == 0 ? vec4(cv, 0.0, 0.0, 1.0) : vec4(0.0, cv, 0.0, 1.0); return; }
  float disc = refine(pq(p, 0, 0), 0.25 * (pq(p, -1, -1) + pq(p, 1, -1) + pq(p, -1, 1) + pq(p, 1, 1)));
  float g0 = g(p, 0, 0);
  float nwGrad = EPS + abs(c(p, -1, -1) - c(p, 1, 1)) + abs(c(p, -1, -1) - c(p, -3, -3)) + abs(g0 - g(p, -2, -2));
  float neGrad = EPS + abs(c(p, 1, -1) - c(p, -1, 1)) + abs(c(p, 1, -1) - c(p, 3, -3)) + abs(g0 - g(p, 2, -2));
  float swGrad = EPS + abs(c(p, 1, -1) - c(p, -1, 1)) + abs(c(p, -1, 1) - c(p, -3, 3)) + abs(g0 - g(p, -2, 2));
  float seGrad = EPS + abs(c(p, -1, -1) - c(p, 1, 1)) + abs(c(p, 1, 1) - c(p, 3, 3)) + abs(g0 - g(p, 2, 2));
  float nwEst = c(p, -1, -1) - g(p, -1, -1);
  float neEst = c(p, 1, -1) - g(p, 1, -1);
  float swEst = c(p, -1, 1) - g(p, -1, 1);
  float seEst = c(p, 1, 1) - g(p, 1, 1);
  float pEst = (nwGrad * seEst + seGrad * nwEst) / (nwGrad + seGrad);
  float qEst = (neGrad * swEst + swGrad * neEst) / (neGrad + swGrad);
  float v = clamp(g0 + (1.0 - disc) * pEst + disc * qEst, 0.0, 1.0);
  o = own == 0 ? vec4(cv, v, 0.0, 1.0) : vec4(v, cv, 0.0, 1.0);
}`;

// Step 4.3: red and blue at green samples, from cardinal colour differences; then the
// colour of every output pixel: camera matrix, half-float encoding and the tone curve
// (the server's 65536-entry table), written into the tile's layer.
const FS_OUT = `${COMMON}
uniform highp sampler2D u_g;
uniform highp sampler2D u_rb;
uniform highp sampler2D u_vh;
uniform highp sampler2D u_lut;
uniform ivec2 u_off;
uniform vec3 u_m0;
uniform vec3 u_m1;
uniform vec3 u_m2;
float g(ivec2 p, int dx, int dy) { return texelFetch(u_g, p + ivec2(dx, dy), 0).r; }
float vh(ivec2 p, int dx, int dy) { return texelFetch(u_vh, p + ivec2(dx, dy), 0).r; }
float x(ivec2 p, int dx, int dy, int k) { vec2 v = texelFetch(u_rb, p + ivec2(dx, dy), 0).rg; return k == 0 ? v.r : v.g; }
float tone(float v) {
  uint b = packHalf2x16(vec2(v, 0.0)) & 0xffffu;
  return texelFetch(u_lut, ivec2(int(b & 255u), int(b >> 8u)), 0).r;
}
void main() {
  ivec2 p = ivec2(gl_FragCoord.xy) + u_off;
  float g0 = g(p, 0, 0);
  vec3 cam;
  if (fc(p) != 1) {
    vec2 rb = texelFetch(u_rb, p, 0).rg;
    cam = vec3(rb.r, g0, rb.g);
  } else {
    float disc = refine(vh(p, 0, 0), 0.25 * (vh(p, -1, -1) + vh(p, 1, -1) + vh(p, -1, 1) + vh(p, 1, 1)));
    float gn = g(p, 0, -2), gs = g(p, 0, 2), gw = g(p, -2, 0), ge = g(p, 2, 0);
    float gn1 = g(p, 0, -1), gs1 = g(p, 0, 1), gw1 = g(p, -1, 0), ge1 = g(p, 1, 0);
    float rb[2];
    for (int k = 0; k < 2; k++) {
      float xn = x(p, 0, -1, k), xs = x(p, 0, 1, k), xw = x(p, -1, 0, k), xe = x(p, 1, 0, k);
      float sn = abs(xn - xs);
      float ew = abs(xw - xe);
      float nGrad = EPS + sn + abs(xn - x(p, 0, -3, k)) + abs(g0 - gn);
      float sGrad = EPS + sn + abs(xs - x(p, 0, 3, k)) + abs(g0 - gs);
      float wGrad = EPS + ew + abs(xw - x(p, -3, 0, k)) + abs(g0 - gw);
      float eGrad = EPS + ew + abs(xe - x(p, 3, 0, k)) + abs(g0 - ge);
      float vEst = (nGrad * (xs - gs1) + sGrad * (xn - gn1)) / (nGrad + sGrad);
      float hEst = (eGrad * (xw - gw1) + wGrad * (xe - ge1)) / (eGrad + wGrad);
      rb[k] = clamp(g0 + (1.0 - disc) * vEst + disc * hEst, 0.0, 1.0);
    }
    cam = vec3(rb[0], g0, rb[1]);
  }
  o = vec4(tone(dot(u_m0, cam)), tone(dot(u_m1, cam)), tone(dot(u_m2, cam)), 1.0);
}`;

type Uniforms = Record<string, WebGLUniformLocation | null>;

/** Texture units used by the passes (the viewer uses 0-4). */
const UNIT = { cfa: 5, vh: 6, lpf: 7, g: 8, pq: 9, rb: 10, lut: 11 } as const;

export class RcdGpu {
  private progs: Record<'vh' | 'lpf' | 'green' | 'pq' | 'rb' | 'out', [WebGLProgram, Uniforms]>;
  private tex: Record<'cfa' | 'vh' | 'lpf' | 'g' | 'pq' | 'rb' | 'lut', WebGLTexture>;
  private fbo: WebGLFramebuffer;
  private max = 0;
  lutKey = '';

  /** Whether this context can run the passes (float render targets). */
  static supported(gl: WebGL2RenderingContext): boolean {
    return !!gl.getExtension('EXT_color_buffer_float') && gl.getParameter(gl.MAX_TEXTURE_IMAGE_UNITS) > UNIT.lut;
  }

  constructor(private gl: WebGL2RenderingContext) {
    if (!RcdGpu.supported(gl)) throw new Error('float render targets are not available');
    const sh = (type: number, src: string) => {
      const s = gl.createShader(type)!;
      gl.shaderSource(s, src);
      gl.compileShader(s);
      if (!gl.getShaderParameter(s, gl.COMPILE_STATUS)) throw new Error(gl.getShaderInfoLog(s) ?? 'shader');
      return s;
    };
    const vs = sh(gl.VERTEX_SHADER, VS);
    const program = (fs: string, samplers: Partial<Record<string, number>>, names: string[]): [WebGLProgram, Uniforms] => {
      const p = gl.createProgram()!;
      gl.attachShader(p, vs);
      gl.attachShader(p, sh(gl.FRAGMENT_SHADER, fs));
      gl.bindAttribLocation(p, 0, 'p');
      gl.linkProgram(p);
      if (!gl.getProgramParameter(p, gl.LINK_STATUS)) throw new Error(gl.getProgramInfoLog(p) ?? 'link');
      const u: Uniforms = {};
      for (const n of ['u_pat', 'u_size', ...names]) u[n] = gl.getUniformLocation(p, n);
      gl.useProgram(p);
      // Every sampler gets its own unit (an unset one would read unit 0).
      for (const [n, unit] of Object.entries({ u_cfa: UNIT.cfa, ...samplers })) gl.uniform1i(gl.getUniformLocation(p, n), unit!);
      return [p, u];
    };
    this.progs = {
      vh: program(FS_VH, {}, []),
      lpf: program(FS_LPF, {}, []),
      green: program(FS_GREEN, { u_vh: UNIT.vh, u_lpf: UNIT.lpf }, []),
      pq: program(FS_PQ, {}, []),
      rb: program(FS_RB, { u_g: UNIT.g, u_pq: UNIT.pq }, []),
      out: program(FS_OUT, { u_g: UNIT.g, u_rb: UNIT.rb, u_vh: UNIT.vh, u_lut: UNIT.lut }, ['u_off', 'u_m0', 'u_m1', 'u_m2']),
    };
    const t = () => gl.createTexture()!;
    this.tex = { cfa: t(), vh: t(), lpf: t(), g: t(), pq: t(), rb: t(), lut: t() };
    this.fbo = gl.createFramebuffer()!;
    gl.activeTexture(gl.TEXTURE0 + UNIT.lut);
    gl.bindTexture(gl.TEXTURE_2D, this.tex.lut);
    gl.texStorage2D(gl.TEXTURE_2D, 1, gl.R8, 256, 256);
    this.nearest();
    gl.activeTexture(gl.TEXTURE0);
  }

  private nearest() {
    const gl = this.gl;
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
  }

  /** Room for inputs up to `n` x `n`. */
  private ensure(n: number) {
    if (n <= this.max) return;
    const gl = this.gl;
    const make = (k: keyof RcdGpu['tex'], unit: number, fmt: number) => {
      gl.deleteTexture(this.tex[k]);
      const tx = gl.createTexture()!;
      this.tex[k] = tx;
      gl.activeTexture(gl.TEXTURE0 + unit);
      gl.bindTexture(gl.TEXTURE_2D, tx);
      gl.texStorage2D(gl.TEXTURE_2D, 1, fmt, n, n);
      this.nearest();
    };
    make('cfa', UNIT.cfa, gl.R16UI);
    make('vh', UNIT.vh, gl.R32F);
    make('lpf', UNIT.lpf, gl.R32F);
    make('g', UNIT.g, gl.R32F);
    make('pq', UNIT.pq, gl.R32F);
    make('rb', UNIT.rb, gl.RG32F);
    gl.activeTexture(gl.TEXTURE0);
    this.max = n;
  }

  /** The view's tone curve: 65536 display values, one per half-float pattern. */
  setLut(lut: Uint8Array, key: string) {
    const gl = this.gl;
    if (lut.length !== 65536) throw new Error('bad tone curve');
    gl.activeTexture(gl.TEXTURE0 + UNIT.lut);
    gl.bindTexture(gl.TEXTURE_2D, this.tex.lut);
    gl.pixelStorei(gl.UNPACK_ALIGNMENT, 1);
    gl.pixelStorei(gl.UNPACK_FLIP_Y_WEBGL, false);
    gl.texSubImage2D(gl.TEXTURE_2D, 0, 0, 0, 256, 256, gl.RED, gl.UNSIGNED_BYTE, lut);
    gl.pixelStorei(gl.UNPACK_ALIGNMENT, 4);
    gl.activeTexture(gl.TEXTURE0);
    this.lutKey = key;
  }

  /** Demosaics `t` and writes its pixels (apron included) through `target`, a
   * framebuffer whose colour attachment is the tile's layer. */
  render(t: CfaTile, target: WebGLFramebuffer) {
    const gl = this.gl;
    const outW = t.tw + 2 * t.apron;
    const outH = t.th + 2 * t.apron;
    if (t.margin < 5 || outW + 2 * t.margin !== t.w || outH + 2 * t.margin !== t.h) throw new Error('unexpected tile geometry');
    this.ensure(Math.max(t.w, t.h));
    // The samples.
    gl.activeTexture(gl.TEXTURE0 + UNIT.cfa);
    gl.bindTexture(gl.TEXTURE_2D, this.tex.cfa);
    gl.pixelStorei(gl.UNPACK_ALIGNMENT, 2);
    gl.pixelStorei(gl.UNPACK_FLIP_Y_WEBGL, false);
    gl.pixelStorei(gl.UNPACK_PREMULTIPLY_ALPHA_WEBGL, false);
    gl.texSubImage2D(gl.TEXTURE_2D, 0, 0, 0, t.w, t.h, gl.RED_INTEGER, gl.UNSIGNED_SHORT, t.data);
    gl.pixelStorei(gl.UNPACK_ALIGNMENT, 4);
    // Every pass's inputs on their units.
    for (const k of ['vh', 'lpf', 'g', 'pq', 'rb'] as const) {
      gl.activeTexture(gl.TEXTURE0 + UNIT[k]);
      gl.bindTexture(gl.TEXTURE_2D, this.tex[k]);
    }
    gl.activeTexture(gl.TEXTURE0 + UNIT.lut);
    gl.bindTexture(gl.TEXTURE_2D, this.tex.lut);
    const pass = (name: keyof RcdGpu['progs'], into: WebGLTexture | null) => {
      const [p, u] = this.progs[name];
      gl.useProgram(p);
      gl.uniform4i(u.u_pat, t.pattern[0], t.pattern[1], t.pattern[2], t.pattern[3]);
      gl.uniform2i(u.u_size, t.w, t.h);
      if (into) {
        gl.bindFramebuffer(gl.FRAMEBUFFER, this.fbo);
        gl.framebufferTexture2D(gl.FRAMEBUFFER, gl.COLOR_ATTACHMENT0, gl.TEXTURE_2D, into, 0);
        gl.viewport(0, 0, t.w, t.h);
      }
      return u;
    };
    // A texture is never read by the pass that writes it: unbind each target from its
    // unit while it is written (WebGL rejects feedback loops).
    const write = (k: 'vh' | 'lpf' | 'g' | 'pq' | 'rb', name: keyof RcdGpu['progs']) => {
      gl.activeTexture(gl.TEXTURE0 + UNIT[k]);
      gl.bindTexture(gl.TEXTURE_2D, null);
      pass(name, this.tex[k]);
      gl.drawArrays(gl.TRIANGLE_STRIP, 0, 4);
      gl.framebufferTexture2D(gl.FRAMEBUFFER, gl.COLOR_ATTACHMENT0, gl.TEXTURE_2D, null, 0);
      gl.activeTexture(gl.TEXTURE0 + UNIT[k]);
      gl.bindTexture(gl.TEXTURE_2D, this.tex[k]);
    };
    write('vh', 'vh');
    write('lpf', 'lpf');
    write('g', 'green');
    write('pq', 'pq');
    write('rb', 'rb');
    // The output, into the tile's layer.
    const u = pass('out', null);
    gl.bindFramebuffer(gl.FRAMEBUFFER, target);
    gl.viewport(0, 0, outW, outH);
    gl.uniform2i(u.u_off, t.margin, t.margin);
    const m = t.matrix;
    gl.uniform3f(u.u_m0, m[0], m[1], m[2]);
    gl.uniform3f(u.u_m1, m[3], m[4], m[5]);
    gl.uniform3f(u.u_m2, m[6], m[7], m[8]);
    gl.drawArrays(gl.TRIANGLE_STRIP, 0, 4);
    gl.bindFramebuffer(gl.FRAMEBUFFER, null);
    gl.activeTexture(gl.TEXTURE0);
  }

  destroy() {
    const gl = this.gl;
    for (const t of Object.values(this.tex)) gl.deleteTexture(t);
    for (const [p] of Object.values(this.progs)) gl.deleteProgram(p);
    gl.deleteFramebuffer(this.fbo);
  }
}
