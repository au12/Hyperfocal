// WebGL2 photo renderer. A full-screen pass handles orientation, zoom/pan, screen
// sharpening, inversion, focus peaking and over/underexposure overlays, so toggling any
// view tool is instant and never touches the server. Screen sharpening adds two cheap
// passes (the view, then a horizontal Gaussian) at canvas resolution; the main pass does
// the vertical half and the unsharp mask.
//
// RAW detail: zoomed in, full-resolution tiles (see detail.ts) replace the half-size
// image where they are loaded. They live in an array texture (one tile per layer, with a
// pixel of the neighbouring tiles around it so filtering is seamless) and a page table
// says which layer holds which tile.

import { tel } from '../telemetry';

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

// The image at a point: a loaded full-resolution tile there, else the base image.
const IMG = `
uniform sampler2D u_img;
uniform highp sampler2DArray u_atlas;
uniform highp usampler2D u_page;  // per tile: atlas layer + 1, or 0
uniform bool u_detail;
uniform vec2 u_full;       // full-resolution size (sensor orientation), px
uniform float u_tile;      // tile size, px
uniform float u_apron;     // neighbouring pixels around each tile in its layer
uniform float u_layer;     // layer size, px
uniform float u_gain;      // exposure correction for images not rendered with it (1 = none)

// Exposure correction in linear light on sRGB display values.
vec3 expose(vec3 c) {
  if (u_gain == 1.0) return c;
  vec3 lin = mix(c / 12.92, pow((c + 0.055) / 1.055, vec3(2.4)), step(0.04045, c));
  lin = min(lin * u_gain, 1.0);
  return mix(lin * 12.92, 1.055 * pow(lin, vec3(1.0 / 2.4)) - 0.055, step(0.0031308, lin));
}

// No implicit-LOD texture() after the tile branch: neighbouring pixels of a 2x2 shading
// quad can fall in different tiles (or one in a tile, one on the base image), and their
// tile coordinates then jump by a whole tile. The derivatives behind texture()'s mip
// choice are undefined there; some drivers (seen on macOS) return wrong texels, which
// showed as 1-px lines on tile seams (made bolder by screen sharpening). Tiles have one
// level, sampled at LOD 0.
//
// The base image's gradients (gx, gy: d st / d screen px) come from the caller, taken
// in uniform control flow before any branch. Taking them here broke the image's edges:
// main() returns early for background pixels, so in every quad straddling the border
// some pixels never computed st, the gradients were garbage and those edge pixels drew
// from an arbitrary mip level (lines along the edges that moved with zoom and pan).
vec3 imgSrc(vec2 st, vec2 gx, vec2 gy, float bias) {
  if (u_detail) {
    vec2 px = clamp(st, 0.0, 1.0) * u_full;
    ivec2 t = min(ivec2(px / u_tile), textureSize(u_page, 0) - 1);
    uint l = texelFetch(u_page, t, 0).r;
    if (l > 0u) {
      vec2 local = px - vec2(t) * u_tile + u_apron;
      return textureLod(u_atlas, vec3(local / u_layer, float(l - 1u)), 0.0).rgb;
    }
  }
  // A LOD bias b is the gradients scaled by 2^b.
  float k = exp2(bias);
  return textureGrad(u_img, st, gx * k, gy * k).rgb;
}

vec3 img(vec2 st, vec2 gx, vec2 gy, float bias) { return expose(imgSrc(st, gx, gy, bias)); }`;

// Pass 1 of screen sharpening: the plain view, edges extended past the image so the blur
// does not pull the background in.
const FS_VIEW = `#version 300 es
precision highp float;
${IMG}
${MAP}
out vec4 outColor;
void main() {
  vec2 d = mapPx(gl_FragCoord.xy);
  // Gradients of the unclamped mapping: past the edges the clamped one is flat (LOD 0).
  vec2 sd = orient(d);
  outColor = vec4(img(orient(clamp(d, 0.0, 1.0)), dFdx(sd), dFdy(sd), LOD_BIAS), 1.0);
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

// Clipping mask (MASK_* bits per pixel) to per-channel coverage: overexposure in one
// texture, underexposure in another (1 = that channel is flagged). Mipmaps of these average
// the flags, so a zoomed-out view reads the share of each footprint that is flagged.
const FS_UNPACK = `#version 300 es
precision highp float;
uniform sampler2D u_src;
layout(location = 0) out vec4 oOver;
layout(location = 1) out vec4 oUnder;
void main() {
  int bits = int(texelFetch(u_src, ivec2(gl_FragCoord.xy), 0).r * 255.0 + 0.5);
  oOver = vec4(float(bits & 1), float((bits >> 1) & 1), float((bits >> 2) & 1), 1.0);
  oUnder = vec4(float((bits >> 3) & 1), float((bits >> 4) & 1), float((bits >> 5) & 1), 1.0);
}`;

const FS = `#version 300 es
precision highp float;
${IMG}
uniform sampler2D u_overCov;  // per-channel overexposure coverage (mipmapped average)
uniform sampler2D u_underCov; // per-channel underexposure coverage
uniform sampler2D u_blurH;  // pass 2 output (horizontal blur of the view)
uniform bool u_hasImg;
uniform bool u_hasMask;
uniform bool u_maskOnly;    // RAW view: clipping comes from the RAW mask only
${MAP}
uniform vec2 u_texel;       // 1 / texture size (of the full-resolution frame with detail on)
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

// A channel is shown where its coverage reaches the cut (FastRawViewer's display, fitted:
// a screen pixel is marked when about half its footprint is flagged; underexposure, being
// speckled shadow noise, from 0.4). Bilinear coverage gives smooth outlines when zoomed in,
// the averaged mipmaps a majority rule when zoomed out; the edge is anti-aliased over about
// a screen pixel. fw is fwidth(cov), taken before any branch. Returns the per-channel
// weights (0..1).
vec3 marks(vec3 cov, vec3 fw, float cut) {
  vec3 w = clamp(fw * 0.75, vec3(0.002), vec3(0.5));
  return smoothstep(vec3(cut) - w, vec3(cut) + w, cov);
}

void main() {
  vec2 d = mapPx(gl_FragCoord.xy);
  // Everything that needs derivatives happens here, before the early return for the
  // background: implicit derivatives (dFdx, fwidth, texture()) are undefined once pixels
  // of a 2x2 quad have taken different branches, which is exactly the image's border.
  // The gradients come from the unclamped mapping (it is affine; the clamped one would
  // halve them on edge pixels whose quad neighbours lie outside).
  vec2 st = orient(clamp(d, 0.0, 1.0));
  vec2 sd = orient(d);
  vec2 gx = dFdx(sd), gy = dFdy(sd);
  vec3 overCov = vec3(0.0), underCov = vec3(0.0), overFw = vec3(0.0), underFw = vec3(0.0);
  if (u_hasMask && (u_over || u_under)) {
    overCov = textureGrad(u_overCov, st, gx, gy).rgb;
    underCov = textureGrad(u_underCov, st, gx, gy).rgb;
    overFw = fwidth(overCov);
    underFw = fwidth(underCov);
  }
  if (!u_hasImg || d.x < 0.0 || d.y < 0.0 || d.x > 1.0 || d.y > 1.0) { outColor = vec4(u_bg, 1.0); return; }
  vec3 c = img(st, gx, gy, LOD_BIAS);

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
    float tl = luma(img(st - tx - ty, gx, gy, 0.0)), t = luma(img(st - ty, gx, gy, 0.0)), tr = luma(img(st + tx - ty, gx, gy, 0.0));
    float l = luma(img(st - tx, gx, gy, 0.0)), m = luma(img(st, gx, gy, 0.0)), r = luma(img(st + tx, gx, gy, 0.0));
    float bl = luma(img(st - tx + ty, gx, gy, 0.0)), bo = luma(img(st + ty, gx, gy, 0.0)), br = luma(img(st + tx + ty, gx, gy, 0.0));
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
      // Underexposure first, overexposure over it. Colours as FastRawViewer: overexposure
      // removes the clipped channels from white (magenta = green, black = all),
      // underexposure adds the underexposed ones to black (blue = blue, white = all);
      // where channels meet, the colours blend across the anti-aliased edge.
      if (u_under) {
        vec3 s = marks(underCov, underFw, 0.4);
        float a = max(s.r, max(s.g, s.b));
        if (a > 0.0) c = mix(c, u_underSolid ? u_underColor : s / a, a);
      }
      if (u_over) {
        vec3 s = marks(overCov, overFw, 0.5);
        float a = max(s.r, max(s.g, s.b));
        if (a > 0.0) c = mix(c, u_overSolid ? vec3(1.0, 0.0, 0.0) : vec3(1.0) - s / a, a);
      }
    } else if (!u_maskOnly) {
      vec3 src = img(st, gx, gy, LOD_BIAS);
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

/** Texture unit of the underexposure coverage: above those GPU demosaicing uses (5-11),
 * so it is rebound every draw anyway. */
const UNDER_UNIT = 12;

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
  /** Exposure gain applied on screen (2^EV), for images the server did not render with
   * the correction (JPEG previews, thumbnails). 1 or absent = none. */
  gain?: number;
}

export function hexToRgb(h: string): [number, number, number] {
  const m = h.replace('#', '').match(/^([0-9a-f]{2})([0-9a-f]{2})([0-9a-f]{2})$/i);
  if (!m) return [1, 0, 0];
  return [parseInt(m[1], 16) / 255, parseInt(m[2], 16) / 255, parseInt(m[3], 16) / 255];
}

type Uniforms = Record<string, WebGLUniformLocation | null>;

/** Every view, for debug mode's estimate of the graphics memory the viewer holds. */
const views = new Set<WeakRef<GLView>>();
tel.gauge('gpu_mb', () => {
  let b = 0;
  for (const r of views) {
    const v = r.deref();
    if (!v) views.delete(r);
    else b += v.memoryBytes();
  }
  return b / 1048576;
});

export class GLView {
  gl: WebGL2RenderingContext;
  private prog: WebGLProgram;
  private progView: WebGLProgram;
  private progBlur: WebGLProgram;
  private progUnpack: WebGLProgram;
  private u: Uniforms;
  private uView: Uniforms;
  private uBlur: Uniforms;
  private uUnpack: Uniforms;
  private tex: WebGLTexture;
  /** Per-channel overexposure / underexposure coverage (from the clipping mask). */
  private overCov: WebGLTexture;
  private underCov: WebGLTexture;
  private fbo: WebGLFramebuffer;
  /** Sharpening targets at canvas size: the view, and its horizontal blur. */
  private targets: { w: number; h: number; view: WebGLTexture; blurH: WebGLTexture } | null = null;
  texW = 0;
  texH = 0;
  maskW = 0;
  maskH = 0;
  hasImg = false;
  hasMask = false;
  maxTex: number;
  maxLayers: number;
  /** Full-resolution tiles: the array texture, its page table and geometry. */
  private atlas: WebGLTexture | null = null;
  private page: WebGLTexture;
  private pageData = new Uint16Array(1);
  private pageW = 1;
  private pageH = 1;
  private pageDirty = true;
  private detailOn = false;
  detail: { full: [number, number]; tile: number; apron: number; layers: number } | null = null;

  constructor(public canvas: HTMLCanvasElement) {
    const gl = canvas.getContext('webgl2', { antialias: false, alpha: false, premultipliedAlpha: false, preserveDrawingBuffer: false });
    if (!gl) throw new Error('WebGL2 is not available');
    this.gl = gl;
    views.add(new WeakRef(this));
    // Not when the view closes: destroy() lets go of the context on purpose.
    canvas.addEventListener('webglcontextlost', () => {
      if (!this.gone) tel.error('webgl', 'WebGL context lost (the graphics driver reset or the browser reclaimed memory)');
    });
    this.maxTex = gl.getParameter(gl.MAX_TEXTURE_SIZE);
    this.maxLayers = gl.getParameter(gl.MAX_ARRAY_TEXTURE_LAYERS);
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
    const imgU = ['u_img', 'u_atlas', 'u_page', 'u_detail', 'u_full', 'u_tile', 'u_apron', 'u_layer', 'u_gain'];
    [this.prog, this.u] = program(FS, [
      ...imgU, 'u_overCov', 'u_underCov', 'u_blurH', 'u_hasImg', 'u_hasMask', 'u_maskOnly', ...mapU, 'u_texel', 'u_sharpen', 'u_sigma', 'u_invert',
      'u_peak', 'u_peakColor', 'u_over', 'u_under', 'u_overSolid', 'u_underSolid', 'u_underColor', 'u_bg',
    ]);
    [this.progView, this.uView] = program(FS_VIEW, [...imgU, ...mapU]);
    [this.progBlur, this.uBlur] = program(FS_BLUR, ['u_src', 'u_sigma']);
    [this.progUnpack, this.uUnpack] = program(FS_UNPACK, ['u_src']);

    const buf = gl.createBuffer();
    gl.bindBuffer(gl.ARRAY_BUFFER, buf);
    gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1, -1, 1, -1, -1, 1, 1, 1]), gl.STATIC_DRAW);
    gl.enableVertexAttribArray(0);
    gl.vertexAttribPointer(0, 2, gl.FLOAT, false, 0, 0);
    this.tex = gl.createTexture()!;
    this.overCov = gl.createTexture()!;
    this.underCov = gl.createTexture()!;
    this.page = gl.createTexture()!;
    this.fbo = gl.createFramebuffer()!;
    gl.useProgram(this.prog);
    gl.uniform1i(this.u.u_img, 0);
    gl.uniform1i(this.u.u_overCov, 1);
    gl.uniform1i(this.u.u_underCov, UNDER_UNIT);
    gl.uniform1i(this.u.u_blurH, 2);
    gl.uniform1i(this.u.u_atlas, 3);
    gl.uniform1i(this.u.u_page, 4);
    gl.useProgram(this.progView);
    gl.uniform1i(this.uView.u_img, 0);
    gl.uniform1i(this.uView.u_atlas, 3);
    gl.uniform1i(this.uView.u_page, 4);
    // Unit 3 always holds an array texture and unit 4 an integer one, so the programs
    // validate before any tile exists.
    gl.activeTexture(gl.TEXTURE3);
    gl.bindTexture(gl.TEXTURE_2D_ARRAY, this.emptyAtlas());
    gl.activeTexture(gl.TEXTURE4);
    this.uploadPage();
    gl.activeTexture(gl.TEXTURE0);
    gl.useProgram(this.progBlur);
    gl.uniform1i(this.uBlur.u_src, 2);
    gl.useProgram(this.progUnpack);
    gl.uniform1i(this.uUnpack.u_src, 1);
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
    const t0 = tel.on ? performance.now() : 0;
    gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA8, gl.RGBA, gl.UNSIGNED_BYTE, src);
    gl.generateMipmap(gl.TEXTURE_2D);
    // Debug mode: the upload as the main thread feels it (a 24 MP image is ~130 MB of mips).
    if (tel.on) tel.time(w * h > 8e6 ? 'gl.upload.large' : 'gl.upload', performance.now() - t0);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR_MIPMAP_LINEAR);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
    this.texW = w;
    this.texH = h;
    this.hasImg = true;
  }

  /** The clipping mask (MASK_* bits per pixel): unpacked on the GPU into per-channel
   * overexposure and underexposure coverage, with averaged mipmaps. */
  setMask(src: (TexImageSource & { width: number; height: number }) | null) {
    const gl = this.gl;
    if (!src) {
      this.hasMask = false;
      return;
    }
    const w = src.width;
    const h = src.height;
    const levels = Math.max(1, Math.floor(Math.log2(Math.max(w, h))) + 1);
    // The bits, read once.
    const bits = gl.createTexture()!;
    gl.activeTexture(gl.TEXTURE1);
    gl.bindTexture(gl.TEXTURE_2D, bits);
    gl.texStorage2D(gl.TEXTURE_2D, 1, gl.RGBA8, w, h);
    gl.pixelStorei(gl.UNPACK_COLORSPACE_CONVERSION_WEBGL, gl.NONE);
    gl.pixelStorei(gl.UNPACK_PREMULTIPLY_ALPHA_WEBGL, false);
    gl.texSubImage2D(gl.TEXTURE_2D, 0, 0, 0, gl.RGBA, gl.UNSIGNED_BYTE, src);
    gl.pixelStorei(gl.UNPACK_COLORSPACE_CONVERSION_WEBGL, gl.BROWSER_DEFAULT_WEBGL);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST);
    // Fresh coverage storage each time: immutable, all levels allocated.
    const cov = () => {
      const t = gl.createTexture()!;
      gl.bindTexture(gl.TEXTURE_2D, t);
      gl.texStorage2D(gl.TEXTURE_2D, levels, gl.RGBA8, w, h);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR_MIPMAP_LINEAR);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
      return t;
    };
    gl.activeTexture(gl.TEXTURE2);
    gl.deleteTexture(this.overCov);
    gl.deleteTexture(this.underCov);
    this.overCov = cov();
    this.underCov = cov();
    gl.activeTexture(gl.TEXTURE1);
    gl.bindTexture(gl.TEXTURE_2D, bits);
    gl.useProgram(this.progUnpack);
    gl.bindFramebuffer(gl.FRAMEBUFFER, this.fbo);
    gl.framebufferTexture2D(gl.FRAMEBUFFER, gl.COLOR_ATTACHMENT0, gl.TEXTURE_2D, this.overCov, 0);
    gl.framebufferTexture2D(gl.FRAMEBUFFER, gl.COLOR_ATTACHMENT1, gl.TEXTURE_2D, this.underCov, 0);
    gl.drawBuffers([gl.COLOR_ATTACHMENT0, gl.COLOR_ATTACHMENT1]);
    gl.viewport(0, 0, w, h);
    gl.drawArrays(gl.TRIANGLE_STRIP, 0, 4);
    // The framebuffer is shared (sharpening, GPU tiles): back to one attachment.
    gl.framebufferTexture2D(gl.FRAMEBUFFER, gl.COLOR_ATTACHMENT1, gl.TEXTURE_2D, null, 0);
    gl.drawBuffers([gl.COLOR_ATTACHMENT0]);
    gl.bindFramebuffer(gl.FRAMEBUFFER, null);
    gl.deleteTexture(bits);
    gl.activeTexture(gl.TEXTURE2);
    for (const t of [this.overCov, this.underCov]) {
      gl.bindTexture(gl.TEXTURE_2D, t);
      gl.generateMipmap(gl.TEXTURE_2D);
    }
    gl.activeTexture(gl.TEXTURE0);
    this.maskW = w;
    this.maskH = h;
    this.hasMask = true;
  }

  private placeholder: WebGLTexture | null = null;
  private emptyAtlas(): WebGLTexture {
    const gl = this.gl;
    if (!this.placeholder) {
      this.placeholder = gl.createTexture()!;
      gl.bindTexture(gl.TEXTURE_2D_ARRAY, this.placeholder);
      gl.texStorage3D(gl.TEXTURE_2D_ARRAY, 1, gl.RGBA8, 1, 1, 1);
    }
    return this.placeholder;
  }

  private uploadPage() {
    const gl = this.gl;
    gl.bindTexture(gl.TEXTURE_2D, this.page);
    gl.pixelStorei(gl.UNPACK_ALIGNMENT, 2);
    gl.texImage2D(gl.TEXTURE_2D, 0, gl.R16UI, this.pageW, this.pageH, 0, gl.RED_INTEGER, gl.UNSIGNED_SHORT, this.pageData);
    gl.pixelStorei(gl.UNPACK_ALIGNMENT, 4);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
    this.pageDirty = false;
  }

  /** Sets up full-resolution tiles: a frame of `full` px (sensor orientation) cut into
   * `tile`-px tiles, each stored with `apron` px of its neighbours, `layers` at most at
   * once. Clears every tile. */
  detailConfigure(full: [number, number], tile: number, apron: number, layers: number) {
    const gl = this.gl;
    layers = Math.max(1, Math.min(layers, this.maxLayers));
    const size = tile + 2 * apron;
    const d = this.detail;
    if (!this.atlas || !d || d.layers !== layers || d.tile !== tile || d.apron !== apron) {
      if (this.atlas) gl.deleteTexture(this.atlas);
      this.atlas = gl.createTexture()!;
      gl.activeTexture(gl.TEXTURE3);
      gl.bindTexture(gl.TEXTURE_2D_ARRAY, this.atlas);
      gl.texStorage3D(gl.TEXTURE_2D_ARRAY, 1, gl.RGBA8, size, size, layers);
      gl.texParameteri(gl.TEXTURE_2D_ARRAY, gl.TEXTURE_MIN_FILTER, gl.LINEAR);
      gl.texParameteri(gl.TEXTURE_2D_ARRAY, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
      gl.texParameteri(gl.TEXTURE_2D_ARRAY, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
      gl.texParameteri(gl.TEXTURE_2D_ARRAY, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
      gl.activeTexture(gl.TEXTURE0);
    }
    this.detail = { full, tile, apron, layers };
    this.pageW = Math.max(1, Math.ceil(full[0] / tile));
    this.pageH = Math.max(1, Math.ceil(full[1] / tile));
    this.pageData = new Uint16Array(this.pageW * this.pageH);
    this.pageDirty = true;
  }

  /** Drops the tiles (keeps the storage). */
  detailClear() {
    this.pageData.fill(0);
    this.pageDirty = true;
  }

  /** Releases the tile storage. */
  detailRelease() {
    if (this.atlas) this.gl.deleteTexture(this.atlas);
    this.atlas = null;
    this.detail = null;
    this.detailOn = false;
    this.pageW = this.pageH = 1;
    this.pageData = new Uint16Array(1);
    this.pageDirty = true;
  }

  /** Whether loaded tiles are drawn. */
  detailEnable(on: boolean) {
    this.detailOn = on && !!this.atlas;
  }

  /** Shows (layer >= 0) or hides (-1) tile (tx, ty). */
  detailSetPage(tx: number, ty: number, layer: number) {
    if (tx < 0 || ty < 0 || tx >= this.pageW || ty >= this.pageH) return;
    this.pageData[ty * this.pageW + tx] = layer + 1;
    this.pageDirty = true;
  }

  /** Copies a decoded tile image (apron included) into a layer. */
  detailUpload(layer: number, src: TexImageSource & { width: number; height: number }) {
    const gl = this.gl;
    if (!this.atlas) return;
    gl.activeTexture(gl.TEXTURE3);
    gl.bindTexture(gl.TEXTURE_2D_ARRAY, this.atlas);
    gl.pixelStorei(gl.UNPACK_FLIP_Y_WEBGL, false);
    gl.pixelStorei(gl.UNPACK_PREMULTIPLY_ALPHA_WEBGL, false);
    gl.pixelStorei(gl.UNPACK_COLORSPACE_CONVERSION_WEBGL, gl.BROWSER_DEFAULT_WEBGL);
    gl.texSubImage3D(gl.TEXTURE_2D_ARRAY, 0, 0, 0, layer, src.width, src.height, 1, gl.RGBA, gl.UNSIGNED_BYTE, src);
    gl.activeTexture(gl.TEXTURE0);
  }

  /** Points the shared framebuffer at a layer, for rendering a tile on the GPU. */
  detailTarget(layer: number): WebGLFramebuffer | null {
    const gl = this.gl;
    if (!this.atlas) return null;
    gl.bindFramebuffer(gl.FRAMEBUFFER, this.fbo);
    gl.framebufferTextureLayer(gl.FRAMEBUFFER, gl.COLOR_ATTACHMENT0, this.atlas, 0, layer);
    return this.fbo;
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

  private setDetail(u: Uniforms, gain = 1) {
    const gl = this.gl;
    gl.uniform1f(u.u_gain, gain);
    const d = this.detail;
    const on = this.detailOn && !!d;
    gl.uniform1i(u.u_detail, on ? 1 : 0);
    if (on && d) {
      gl.uniform2f(u.u_full, d.full[0], d.full[1]);
      gl.uniform1f(u.u_tile, d.tile);
      gl.uniform1f(u.u_apron, d.apron);
      gl.uniform1f(u.u_layer, d.tile + 2 * d.apron);
    }
  }

  private setMap(u: Uniforms, v: ViewParams) {
    const gl = this.gl;
    gl.uniform2f(u.u_canvas, this.canvas.width, this.canvas.height);
    gl.uniform2f(u.u_disp, v.dispW, v.dispH);
    gl.uniform2f(u.u_center, v.cx, v.cy);
    gl.uniform1f(u.u_scale, v.scale);
    gl.uniform1i(u.u_orient, v.orientation);
  }

  /** Graphics memory held (an estimate: textures and render targets, mipmaps included). */
  memoryBytes(): number {
    if (this.gone) return 0;
    const mips = 4 / 3;
    let b = this.hasImg ? this.texW * this.texH * 4 * mips : 0;
    if (this.hasMask) b += this.maskW * this.maskH * 4 * mips * 2;
    const d = this.detail;
    if (d && this.atlas) b += (d.tile + 2 * d.apron) ** 2 * 4 * d.layers;
    if (this.targets) b += this.targets.w * this.targets.h * 4 * 2;
    return b + this.pageW * this.pageH * 2 + this.canvas.width * this.canvas.height * 4 * 2;
  }

  draw(v: ViewParams) {
    tel.count('gl.draw');
    const gl = this.gl;
    const u = this.u;
    const { width: cw, height: ch } = this.canvas;
    gl.activeTexture(gl.TEXTURE4);
    if (this.pageDirty) this.uploadPage();
    else gl.bindTexture(gl.TEXTURE_2D, this.page);
    gl.activeTexture(gl.TEXTURE3);
    gl.bindTexture(gl.TEXTURE_2D_ARRAY, this.atlas ?? this.emptyAtlas());
    gl.activeTexture(gl.TEXTURE0 + UNDER_UNIT);
    gl.bindTexture(gl.TEXTURE_2D, this.underCov);
    gl.activeTexture(gl.TEXTURE1);
    gl.bindTexture(gl.TEXTURE_2D, this.overCov);
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
      this.setDetail(this.uView, v.gain);
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
    this.setDetail(u, v.gain);
    gl.uniform1i(u.u_hasImg, this.hasImg ? 1 : 0);
    gl.uniform1i(u.u_hasMask, this.hasMask ? 1 : 0);
    gl.uniform1i(u.u_maskOnly, v.maskOnly ? 1 : 0);
    if (this.detailOn && this.detail) gl.uniform2f(u.u_texel, 1 / this.detail.full[0], 1 / this.detail.full[1]);
    else gl.uniform2f(u.u_texel, 1 / Math.max(1, this.texW), 1 / Math.max(1, this.texH));
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

  private gone = false;

  destroy() {
    this.gone = true;
    const gl = this.gl;
    gl.deleteTexture(this.tex);
    gl.deleteTexture(this.overCov);
    gl.deleteTexture(this.underCov);
    gl.deleteTexture(this.page);
    if (this.atlas) gl.deleteTexture(this.atlas);
    if (this.placeholder) gl.deleteTexture(this.placeholder);
    if (this.targets) {
      gl.deleteTexture(this.targets.view);
      gl.deleteTexture(this.targets.blurH);
    }
    gl.deleteFramebuffer(this.fbo);
    for (const p of [this.prog, this.progView, this.progBlur, this.progUnpack]) gl.deleteProgram(p);
    gl.getExtension('WEBGL_lose_context')?.loseContext();
  }
}
