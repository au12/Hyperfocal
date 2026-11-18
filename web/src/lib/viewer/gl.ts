// WebGL2 photo renderer. A full-screen pass handles orientation, zoom/pan, screen
// sharpening, inversion, focus peaking and over/underexposure overlays, so toggling any
// view tool is instant and never touches the server. Screen sharpening adds two cheap
// passes (the view, then a horizontal Gaussian) at canvas resolution; the main pass does
// the vertical half and the unsharp mask.

const VS = `#version 300 es
in vec2 p;
void main() { gl_Position = vec4(p, 0.0, 1.0); }`;

// Shared: displayed-image mapping of a fragment and EXIF orientation.
const MAP = `
uniform vec2 u_canvas;      // device px
uniform vec2 u_disp;        // displayed (oriented) image size in image px
uniform vec2 u_center;      // view centre in displayed image px
uniform float u_scale;      // device px per image px
uniform int u_orient;

// Image samples lean towards the sharper mip level: trilinear filtering at a mild
// minification (a 2.7K render shown 2.4K wide) otherwise blends in the half-size level,
// leaving the view noticeably softer than FastRawViewer's at the same zoom.
const float LOD_BIAS = -0.5;

vec2 orient(vec2 d) {
  if (u_orient == 2) return vec2(1.0 - d.x, d.y);
  if (u_orient == 3) return vec2(1.0 - d.x, 1.0 - d.y);
  if (u_orient == 4) return vec2(d.x, 1.0 - d.y);
  if (u_orient == 5) return vec2(d.y, d.x);
  if (u_orient == 6) return vec2(d.y, 1.0 - d.x);
  if (u_orient == 7) return vec2(1.0 - d.y, 1.0 - d.x);
  if (u_orient == 8) return vec2(1.0 - d.y, d.x);
  return d;
}

vec2 mapPx(vec2 frag) {
  vec2 s = vec2(frag.x, u_canvas.y - frag.y);
  vec2 img = u_center + (s - 0.5 * u_canvas) / u_scale;
  return img / u_disp;
}`;

// Pass 1 of screen sharpening: the plain view, edges extended past the image so the blur
// does not pull the background in.
const FS_VIEW = `#version 300 es
precision highp float;
uniform sampler2D u_img;
${MAP}
out vec4 outColor;
void main() {
  outColor = vec4(texture(u_img, orient(clamp(mapPx(gl_FragCoord.xy), 0.0, 1.0)), LOD_BIAS).rgb, 1.0);
}`;

// Pass 2: horizontal Gaussian of pass 1.
const FS_BLUR = `#version 300 es
precision highp float;
uniform sampler2D u_src;
uniform float u_sigma;
out vec4 outColor;
void main() {
  ivec2 p = ivec2(gl_FragCoord.xy);
  int w = textureSize(u_src, 0).x;
  int r = int(ceil(3.0 * u_sigma));
  vec3 s = vec3(0.0);
  float ws = 0.0;
  for (int i = -12; i <= 12; i++) {
    if (i < -r || i > r) continue;
    float k = exp(-float(i * i) / (2.0 * u_sigma * u_sigma));
    s += k * texelFetch(u_src, ivec2(clamp(p.x + i, 0, w - 1), p.y), 0).rgb;
    ws += k;
  }
  outColor = vec4(s / ws, 1.0);
}`;

// Mask pyramid: each level ORs the clipping bits of the level below, so a zoomed-out view
// still shows every clipped sample, as FastRawViewer does.
const FS_REDUCE = `#version 300 es
precision highp float;
uniform sampler2D u_src;   // base level = the level below the one being written
out vec4 outColor;
void main() {
  ivec2 size = textureSize(u_src, 0);
  ivec2 q = ivec2(gl_FragCoord.xy);
  ivec2 p = q * 2;
  // 2x2 texels; the last row/column of the level written also takes an odd one left over.
  ivec2 last = max(size / 2, ivec2(1)) - 1;
  ivec2 hi = min(ivec2(q.x == last.x ? size.x - 1 : p.x + 1, q.y == last.y ? size.y - 1 : p.y + 1), size - 1);
  int bits = 0;
  for (int y = p.y; y <= hi.y; y++)
    for (int x = p.x; x <= hi.x; x++)
      bits |= int(texelFetch(u_src, ivec2(x, y), 0).r * 255.0 + 0.5);
  outColor = vec4(float(bits) / 255.0, 0.0, 0.0, 1.0);
}`;

const FS = `#version 300 es
precision highp float;
uniform sampler2D u_img;
uniform sampler2D u_mask;
uniform sampler2D u_blurH;  // pass 2 output (horizontal blur of the view)
uniform bool u_hasImg;
uniform bool u_hasMask;
uniform bool u_maskOnly;    // RAW view: clipping comes from the RAW mask only
uniform float u_overLod;    // mask pyramid levels: clipping one level coarser than a device
uniform float u_underLod;   // px (bold, like FastRawViewer); underexposure at the footprint
${MAP}
uniform vec2 u_texel;       // 1 / texture size
uniform float u_sharpen;    // unsharp amount (0 = off)
uniform float u_sigma;      // unsharp radius in device px
uniform bool u_invert;
uniform int u_peak;         // 0 off, 1 edges, 2 details, 3 both
uniform vec3 u_peakColor;
uniform bool u_over;
uniform bool u_under;
uniform bool u_overSolid;
uniform bool u_underSolid;
uniform vec3 u_underColor;
uniform vec3 u_bg;
out vec4 outColor;

float luma(vec3 c) { return dot(c, vec3(0.2126, 0.7152, 0.0722)); }

// Per-channel colours: clipped channels are removed from white (cyan = red clipped,
// black = all), underexposed ones are added to black (blue = blue under, white = all).
vec3 overColor(int ob) {
  if (u_overSolid) return vec3(1.0, 0.0, 0.0);
  return vec3(1.0) - vec3(float(ob & 1), float((ob >> 1) & 1), float((ob >> 2) & 1));
}
vec3 underColor(int ub) {
  if (u_underSolid) return u_underColor;
  return vec3(float(ub & 1), float((ub >> 1) & 1), float((ub >> 2) & 1));
}

void main() {
  vec2 d = mapPx(gl_FragCoord.xy);
  if (!u_hasImg || d.x < 0.0 || d.y < 0.0 || d.x > 1.0 || d.y > 1.0) { outColor = vec4(u_bg, 1.0); return; }
  vec2 st = orient(d);
  vec3 c = texture(u_img, st, LOD_BIAS).rgb;

  if (u_sharpen > 0.0) {
    // Vertical half of the Gaussian over pass 2, then the unsharp mask (display values).
    ivec2 p = ivec2(gl_FragCoord.xy);
    int h = textureSize(u_blurH, 0).y;
    int r = int(ceil(3.0 * u_sigma));
    vec3 b = vec3(0.0);
    float ws = 0.0;
    for (int i = -12; i <= 12; i++) {
      if (i < -r || i > r) continue;
      float k = exp(-float(i * i) / (2.0 * u_sigma * u_sigma));
      b += k * texelFetch(u_blurH, ivec2(p.x, clamp(p.y + i, 0, h - 1)), 0).rgb;
      ws += k;
    }
    c = clamp(c + u_sharpen * (c - b / ws), 0.0, 1.0);
  }

  if (u_invert) c = vec3(1.0) - c;

  if (u_peak > 0) {
    vec2 tx = vec2(u_texel.x, 0.0);
    vec2 ty = vec2(0.0, u_texel.y);
    float tl = luma(texture(u_img, st - tx - ty).rgb), t = luma(texture(u_img, st - ty).rgb), tr = luma(texture(u_img, st + tx - ty).rgb);
    float l = luma(texture(u_img, st - tx).rgb), m = luma(texture(u_img, st).rgb), r = luma(texture(u_img, st + tx).rgb);
    float bl = luma(texture(u_img, st - tx + ty).rgb), bo = luma(texture(u_img, st + ty).rgb), br = luma(texture(u_img, st + tx + ty).rgb);
    float gx = (tr + 2.0 * r + br) - (tl + 2.0 * l + bl);
    float gy = (bl + 2.0 * bo + br) - (tl + 2.0 * t + tr);
    float g = sqrt(gx * gx + gy * gy);
    float lap = abs(4.0 * m - l - r - t - bo);
    bool edge = g > 0.55;
    bool detail = lap > 0.16 && !edge;
    if ((u_peak == 1 || u_peak == 3) && edge) c = u_peakColor;
    else if ((u_peak == 2 || u_peak == 3) && detail) c = vec3(1.0, 0.9, 0.1);
  }

  if (u_over || u_under) {
    if (u_hasMask) {
      int ob = int(textureLod(u_mask, st, u_overLod).r * 255.0 + 0.5) & 7;
      int ub = (int(textureLod(u_mask, st, u_underLod).r * 255.0 + 0.5) >> 3) & 7;
      if (u_over && ob != 0) c = overColor(ob);
      else if (u_under && ub != 0) c = underColor(ub);
    } else if (!u_maskOnly) {
      vec3 src = texture(u_img, st, LOD_BIAS).rgb;
      vec3 cl = step(vec3(0.996), src);
      vec3 cu = vec3(1.0) - step(vec3(0.008), src);
      int ob = int(cl.r) | (int(cl.g) << 1) | (int(cl.b) << 2);
      int ub = int(cu.r) | (int(cu.g) << 1) | (int(cu.b) << 2);
      if (u_over && ob != 0) c = overColor(ob);
      else if (u_under && ub != 0) c = underColor(ub);
    }
  }
  outColor = vec4(c, 1.0);
}`;

export interface ViewParams {
  orientation: number;
  dispW: number; // displayed (oriented) full-resolution size
  dispH: number;
  scale: number;
  cx: number;
  cy: number;
  /** Unsharp-mask amount (0 = off) and radius in device px. */
  sharpen: number;
  sharpenSigma: number;
  invert: boolean;
  peaking: number;
  peakColor: [number, number, number];
  over: boolean;
  under: boolean;
  /** Over/under come from the RAW mask only; never approximate them from the shown image. */
  maskOnly: boolean;
  overSolid: boolean;
  /** Underexposure in one colour instead of per channel. */
  underSolid: boolean;
  underColor: [number, number, number];
  bg: [number, number, number];
}

export function hexToRgb(h: string): [number, number, number] {
  const m = h.replace('#', '').match(/^([0-9a-f]{2})([0-9a-f]{2})([0-9a-f]{2})$/i);
  if (!m) return [1, 0, 0];
  return [parseInt(m[1], 16) / 255, parseInt(m[2], 16) / 255, parseInt(m[3], 16) / 255];
}

type Uniforms = Record<string, WebGLUniformLocation | null>;

export class GLView {
  gl: WebGL2RenderingContext;
  private prog: WebGLProgram;
  private progView: WebGLProgram;
  private progBlur: WebGLProgram;
  private progReduce: WebGLProgram;
  private u: Uniforms;
  private uView: Uniforms;
  private uBlur: Uniforms;
  private uReduce: Uniforms;
  private tex: WebGLTexture;
  private mask: WebGLTexture;
  private fbo: WebGLFramebuffer;
  /** Sharpening targets at canvas size: the view, and its horizontal blur. */
  private targets: { w: number; h: number; view: WebGLTexture; blurH: WebGLTexture } | null = null;
  texW = 0;
  texH = 0;
  maskW = 0;
  maskH = 0;
  maskLevels = 1;
  hasImg = false;
  hasMask = false;
  maxTex: number;

  constructor(public canvas: HTMLCanvasElement) {
    const gl = canvas.getContext('webgl2', { antialias: false, alpha: false, premultipliedAlpha: false, preserveDrawingBuffer: false });
    if (!gl) throw new Error('WebGL2 is not available');
    this.gl = gl;
    this.maxTex = gl.getParameter(gl.MAX_TEXTURE_SIZE);
    const sh = (type: number, src: string) => {
      const s = gl.createShader(type)!;
      gl.shaderSource(s, src);
      gl.compileShader(s);
      if (!gl.getShaderParameter(s, gl.COMPILE_STATUS)) throw new Error(gl.getShaderInfoLog(s) ?? 'shader');
      return s;
    };
    const vs = sh(gl.VERTEX_SHADER, VS);
    const program = (fs: string, names: string[]): [WebGLProgram, Uniforms] => {
      const p = gl.createProgram()!;
      gl.attachShader(p, vs);
      gl.attachShader(p, sh(gl.FRAGMENT_SHADER, fs));
      gl.bindAttribLocation(p, 0, 'p');
      gl.linkProgram(p);
      if (!gl.getProgramParameter(p, gl.LINK_STATUS)) throw new Error(gl.getProgramInfoLog(p) ?? 'link');
      const u: Uniforms = {};
      for (const n of names) u[n] = gl.getUniformLocation(p, n);
      return [p, u];
    };
    const mapU = ['u_canvas', 'u_disp', 'u_center', 'u_scale', 'u_orient'];
    [this.prog, this.u] = program(FS, [
      'u_img', 'u_mask', 'u_blurH', 'u_hasImg', 'u_hasMask', 'u_maskOnly', 'u_overLod', 'u_underLod', ...mapU, 'u_texel', 'u_sharpen', 'u_sigma', 'u_invert',
      'u_peak', 'u_peakColor', 'u_over', 'u_under', 'u_overSolid', 'u_underSolid', 'u_underColor', 'u_bg',
    ]);
    [this.progView, this.uView] = program(FS_VIEW, ['u_img', ...mapU]);
    [this.progBlur, this.uBlur] = program(FS_BLUR, ['u_src', 'u_sigma']);
    [this.progReduce, this.uReduce] = program(FS_REDUCE, ['u_src']);

    const buf = gl.createBuffer();
    gl.bindBuffer(gl.ARRAY_BUFFER, buf);
    gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1, -1, 1, -1, -1, 1, 1, 1]), gl.STATIC_DRAW);
    gl.enableVertexAttribArray(0);
    gl.vertexAttribPointer(0, 2, gl.FLOAT, false, 0, 0);
    this.tex = gl.createTexture()!;
    this.mask = gl.createTexture()!;
    this.fbo = gl.createFramebuffer()!;
    gl.useProgram(this.prog);
    gl.uniform1i(this.u.u_img, 0);
    gl.uniform1i(this.u.u_mask, 1);
    gl.uniform1i(this.u.u_blurH, 2);
    gl.useProgram(this.progView);
    gl.uniform1i(this.uView.u_img, 0);
    gl.useProgram(this.progBlur);
    gl.uniform1i(this.uBlur.u_src, 2);
    gl.useProgram(this.progReduce);
    gl.uniform1i(this.uReduce.u_src, 1);
  }

  setImage(src: TexImageSource | null, w = 0, h = 0) {
    const gl = this.gl;
    gl.activeTexture(gl.TEXTURE0);
    gl.bindTexture(gl.TEXTURE_2D, this.tex);
    if (!src) {
      this.hasImg = false;
      return;
    }
    gl.pixelStorei(gl.UNPACK_FLIP_Y_WEBGL, false);
    gl.pixelStorei(gl.UNPACK_PREMULTIPLY_ALPHA_WEBGL, false);
    gl.pixelStorei(gl.UNPACK_COLORSPACE_CONVERSION_WEBGL, gl.BROWSER_DEFAULT_WEBGL);
    gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA8, gl.RGBA, gl.UNSIGNED_BYTE, src);
    gl.generateMipmap(gl.TEXTURE_2D);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR_MIPMAP_LINEAR);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
    this.texW = w;
    this.texH = h;
    this.hasImg = true;
  }

  /** The clipping mask (MASK_* bits per pixel), with an OR-reduced pyramid built on the GPU. */
  setMask(src: (TexImageSource & { width: number; height: number }) | null) {
    const gl = this.gl;
    gl.activeTexture(gl.TEXTURE1);
    gl.bindTexture(gl.TEXTURE_2D, this.mask);
    if (!src) {
      this.hasMask = false;
      gl.activeTexture(gl.TEXTURE0);
      return;
    }
    const w = src.width;
    const h = src.height;
    const levels = Math.max(1, Math.floor(Math.log2(Math.max(w, h))) + 1);
    // Fresh storage each time: immutable, all levels allocated.
    gl.deleteTexture(this.mask);
    this.mask = gl.createTexture()!;
    gl.bindTexture(gl.TEXTURE_2D, this.mask);
    gl.texStorage2D(gl.TEXTURE_2D, levels, gl.RGBA8, w, h);
    gl.pixelStorei(gl.UNPACK_COLORSPACE_CONVERSION_WEBGL, gl.NONE);
    gl.texSubImage2D(gl.TEXTURE_2D, 0, 0, 0, gl.RGBA, gl.UNSIGNED_BYTE, src);
    gl.pixelStorei(gl.UNPACK_COLORSPACE_CONVERSION_WEBGL, gl.BROWSER_DEFAULT_WEBGL);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST_MIPMAP_NEAREST);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
    gl.useProgram(this.progReduce);
    gl.bindFramebuffer(gl.FRAMEBUFFER, this.fbo);
    for (let l = 1; l < levels; l++) {
      // Sample only level l-1 while writing level l (no feedback loop).
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_BASE_LEVEL, l - 1);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAX_LEVEL, l - 1);
      gl.framebufferTexture2D(gl.FRAMEBUFFER, gl.COLOR_ATTACHMENT0, gl.TEXTURE_2D, this.mask, l);
      gl.viewport(0, 0, Math.max(1, w >> l), Math.max(1, h >> l));
      gl.drawArrays(gl.TRIANGLE_STRIP, 0, 4);
    }
    gl.bindFramebuffer(gl.FRAMEBUFFER, null);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_BASE_LEVEL, 0);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAX_LEVEL, levels - 1);
    gl.activeTexture(gl.TEXTURE0);
    this.maskW = w;
    this.maskH = h;
    this.maskLevels = levels;
    this.hasMask = true;
  }

  resize(): boolean {
    const dpr = window.devicePixelRatio || 1;
    const w = Math.max(1, Math.round(this.canvas.clientWidth * dpr));
    const h = Math.max(1, Math.round(this.canvas.clientHeight * dpr));
    if (this.canvas.width !== w || this.canvas.height !== h) {
      this.canvas.width = w;
      this.canvas.height = h;
      return true;
    }
    return false;
  }

  private ensureTargets() {
    const gl = this.gl;
    const { width: w, height: h } = this.canvas;
    if (this.targets && this.targets.w === w && this.targets.h === h) return this.targets;
    if (this.targets) {
      gl.deleteTexture(this.targets.view);
      gl.deleteTexture(this.targets.blurH);
    }
    const make = () => {
      const t = gl.createTexture()!;
      gl.bindTexture(gl.TEXTURE_2D, t);
      gl.texStorage2D(gl.TEXTURE_2D, 1, gl.RGBA8, w, h);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
      return t;
    };
    gl.activeTexture(gl.TEXTURE2);
    this.targets = { w, h, view: make(), blurH: make() };
    gl.activeTexture(gl.TEXTURE0);
    return this.targets;
  }

  private setMap(u: Uniforms, v: ViewParams) {
    const gl = this.gl;
    gl.uniform2f(u.u_canvas, this.canvas.width, this.canvas.height);
    gl.uniform2f(u.u_disp, v.dispW, v.dispH);
    gl.uniform2f(u.u_center, v.cx, v.cy);
    gl.uniform1f(u.u_scale, v.scale);
    gl.uniform1i(u.u_orient, v.orientation);
  }

  draw(v: ViewParams) {
    const gl = this.gl;
    const u = this.u;
    const { width: cw, height: ch } = this.canvas;
    gl.activeTexture(gl.TEXTURE1);
    gl.bindTexture(gl.TEXTURE_2D, this.mask);
    gl.activeTexture(gl.TEXTURE0);
    gl.bindTexture(gl.TEXTURE_2D, this.tex);
    const sharpen = v.sharpen > 0 && this.hasImg;
    const sigma = Math.min(4, Math.max(0.3, v.sharpenSigma));
    if (sharpen) {
      const t = this.ensureTargets();
      gl.bindFramebuffer(gl.FRAMEBUFFER, this.fbo);
      gl.viewport(0, 0, cw, ch);
      // Pass 1: the view.
      gl.framebufferTexture2D(gl.FRAMEBUFFER, gl.COLOR_ATTACHMENT0, gl.TEXTURE_2D, t.view, 0);
      gl.useProgram(this.progView);
      this.setMap(this.uView, v);
      gl.drawArrays(gl.TRIANGLE_STRIP, 0, 4);
      // Pass 2: horizontal blur of pass 1.
      gl.framebufferTexture2D(gl.FRAMEBUFFER, gl.COLOR_ATTACHMENT0, gl.TEXTURE_2D, t.blurH, 0);
      gl.activeTexture(gl.TEXTURE2);
      gl.bindTexture(gl.TEXTURE_2D, t.view);
      gl.useProgram(this.progBlur);
      gl.uniform1f(this.uBlur.u_sigma, sigma);
      gl.drawArrays(gl.TRIANGLE_STRIP, 0, 4);
      gl.bindFramebuffer(gl.FRAMEBUFFER, null);
      gl.bindTexture(gl.TEXTURE_2D, t.blurH);
      gl.activeTexture(gl.TEXTURE0);
    }
    gl.viewport(0, 0, cw, ch);
    gl.useProgram(this.prog);
    this.setMap(u, v);
    gl.uniform1i(u.u_hasImg, this.hasImg ? 1 : 0);
    gl.uniform1i(u.u_hasMask, this.hasMask ? 1 : 0);
    gl.uniform1i(u.u_maskOnly, v.maskOnly ? 1 : 0);
    // Mask texels per device pixel: mask texels per image pixel (sensor orientation) over
    // device pixels per image pixel. Clipped samples are usually few and scattered, so when
    // zoomed out they are drawn bold, from the pyramid level whose blocks OR at least the
    // footprint and are at least 1.25 CSS px wide (calibrated on FastRawViewer, whose marks
    // are as bold); exact pixels from about 1:1 up. Underexposure is sampled at the
    // footprint, which matches FastRawViewer as is.
    const sensorW = v.orientation >= 5 ? v.dispH : v.dispW;
    const perPx = this.maskW > 0 && sensorW > 0 ? this.maskW / sensorW / Math.max(1e-6, v.scale) : 1;
    const lvl = Math.log2(Math.max(1e-6, perPx));
    const top = this.maskLevels - 1;
    const dpr = window.devicePixelRatio || 1;
    const bold = Math.max(Math.ceil(lvl - 0.05), Math.ceil(Math.log2(1.25 * dpr * perPx)));
    gl.uniform1f(u.u_overLod, perPx > 0.75 ? Math.min(top, Math.max(0, bold)) : 0);
    gl.uniform1f(u.u_underLod, Math.min(top, Math.max(0, Math.floor(lvl))));
    gl.uniform2f(u.u_texel, 1 / Math.max(1, this.texW), 1 / Math.max(1, this.texH));
    gl.uniform1f(u.u_sharpen, sharpen ? v.sharpen : 0);
    gl.uniform1f(u.u_sigma, sigma);
    gl.uniform1i(u.u_invert, v.invert ? 1 : 0);
    gl.uniform1i(u.u_peak, v.peaking);
    gl.uniform3fv(u.u_peakColor, v.peakColor);
    gl.uniform1i(u.u_over, v.over ? 1 : 0);
    gl.uniform1i(u.u_under, v.under ? 1 : 0);
    gl.uniform1i(u.u_overSolid, v.overSolid ? 1 : 0);
    gl.uniform1i(u.u_underSolid, v.underSolid ? 1 : 0);
    gl.uniform3fv(u.u_underColor, v.underColor);
    gl.uniform3fv(u.u_bg, v.bg);
    gl.drawArrays(gl.TRIANGLE_STRIP, 0, 4);
  }

  destroy() {
    const gl = this.gl;
    gl.deleteTexture(this.tex);
    gl.deleteTexture(this.mask);
    if (this.targets) {
      gl.deleteTexture(this.targets.view);
      gl.deleteTexture(this.targets.blurH);
    }
    gl.deleteFramebuffer(this.fbo);
    for (const p of [this.prog, this.progView, this.progBlur, this.progReduce]) gl.deleteProgram(p);
    gl.getExtension('WEBGL_lose_context')?.loseContext();
  }
}
