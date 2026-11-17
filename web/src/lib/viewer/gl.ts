// WebGL2 photo renderer. One full-screen pass handles orientation, zoom/pan,
// screen sharpening, focus peaking and over/underexposure overlays, so toggling any
// view tool is instant and never touches the server.

const VS = `#version 300 es
in vec2 p;
void main() { gl_Position = vec4(p, 0.0, 1.0); }`;

const FS = `#version 300 es
precision highp float;
uniform sampler2D u_img;
uniform sampler2D u_mask;
uniform bool u_hasImg;
uniform bool u_hasMask;
uniform vec2 u_canvas;      // device px
uniform vec2 u_disp;        // displayed (oriented) image size in image px
uniform vec2 u_center;      // view centre in displayed image px
uniform float u_scale;      // device px per image px
uniform int u_orient;
uniform vec2 u_texel;       // 1 / texture size
uniform float u_sharpen;    // unsharp amount (0 = off)
uniform int u_peak;         // 0 off, 1 edges, 2 details, 3 both
uniform vec3 u_peakColor;
uniform bool u_over;
uniform bool u_under;
uniform bool u_overSolid;
uniform vec3 u_underColor;
uniform vec3 u_bg;
out vec4 outColor;

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
}

float luma(vec3 c) { return dot(c, vec3(0.2126, 0.7152, 0.0722)); }

void main() {
  vec2 d = mapPx(gl_FragCoord.xy);
  if (!u_hasImg || d.x < 0.0 || d.y < 0.0 || d.x > 1.0 || d.y > 1.0) { outColor = vec4(u_bg, 1.0); return; }
  vec2 st = orient(d);
  vec3 c = texture(u_img, st).rgb;

  if (u_sharpen > 0.0) {
    // One screen pixel in texture space, in each direction.
    vec2 ex = orient(mapPx(gl_FragCoord.xy + vec2(1.0, 0.0))) - st;
    vec2 ey = orient(mapPx(gl_FragCoord.xy + vec2(0.0, 1.0))) - st;
    vec3 b = (texture(u_img, st + ex).rgb + texture(u_img, st - ex).rgb + texture(u_img, st + ey).rgb + texture(u_img, st - ey).rgb) * 0.125
           + (texture(u_img, st + ex + ey).rgb + texture(u_img, st - ex - ey).rgb + texture(u_img, st + ex - ey).rgb + texture(u_img, st - ex + ey).rgb) * 0.0625
           + c * 0.25;
    c = clamp(c + u_sharpen * (c - b), 0.0, 1.0);
  }

  if (u_peak > 0) {
    vec2 tx = vec2(u_texel.x, 0.0);
    vec2 ty = vec2(0.0, u_texel.y);
    float tl = luma(texture(u_img, st - tx - ty).rgb), t = luma(texture(u_img, st - ty).rgb), tr = luma(texture(u_img, st + tx - ty).rgb);
    float l = luma(texture(u_img, st - tx).rgb), m = luma(c), r = luma(texture(u_img, st + tx).rgb);
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
      int bits = int(texture(u_mask, st).r * 255.0 + 0.5);
      int ob = bits & 7;
      if (u_over && ob != 0) {
        vec3 clipped = vec3(float(ob & 1), float((ob >> 1) & 1), float((ob >> 2) & 1));
        c = u_overSolid ? vec3(1.0, 0.0, 0.0) : (vec3(1.0) - clipped);
      } else if (u_under && (bits & 8) != 0) {
        c = u_underColor;
      }
    } else {
      vec3 cl = step(vec3(0.996), texture(u_img, st).rgb);
      if (u_over && (cl.r + cl.g + cl.b) > 0.0) c = u_overSolid ? vec3(1.0, 0.0, 0.0) : (vec3(1.0) - cl);
      else if (u_under && max(c.r, max(c.g, c.b)) < 0.008) c = u_underColor;
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
  sharpen: number;
  peaking: number;
  peakColor: [number, number, number];
  over: boolean;
  under: boolean;
  overSolid: boolean;
  underColor: [number, number, number];
  bg: [number, number, number];
}

export function hexToRgb(h: string): [number, number, number] {
  const m = h.replace('#', '').match(/^([0-9a-f]{2})([0-9a-f]{2})([0-9a-f]{2})$/i);
  if (!m) return [1, 0, 0];
  return [parseInt(m[1], 16) / 255, parseInt(m[2], 16) / 255, parseInt(m[3], 16) / 255];
}

export class GLView {
  gl: WebGL2RenderingContext;
  private prog: WebGLProgram;
  private u: Record<string, WebGLUniformLocation | null> = {};
  private tex: WebGLTexture;
  private mask: WebGLTexture;
  texW = 0;
  texH = 0;
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
    const p = gl.createProgram()!;
    gl.attachShader(p, sh(gl.VERTEX_SHADER, VS));
    gl.attachShader(p, sh(gl.FRAGMENT_SHADER, FS));
    gl.linkProgram(p);
    if (!gl.getProgramParameter(p, gl.LINK_STATUS)) throw new Error(gl.getProgramInfoLog(p) ?? 'link');
    this.prog = p;
    gl.useProgram(p);
    const buf = gl.createBuffer();
    gl.bindBuffer(gl.ARRAY_BUFFER, buf);
    gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1, -1, 1, -1, -1, 1, 1, 1]), gl.STATIC_DRAW);
    const loc = gl.getAttribLocation(p, 'p');
    gl.enableVertexAttribArray(loc);
    gl.vertexAttribPointer(loc, 2, gl.FLOAT, false, 0, 0);
    for (const n of ['u_img', 'u_mask', 'u_hasImg', 'u_hasMask', 'u_canvas', 'u_disp', 'u_center', 'u_scale', 'u_orient', 'u_texel', 'u_sharpen', 'u_peak', 'u_peakColor', 'u_over', 'u_under', 'u_overSolid', 'u_underColor', 'u_bg']) {
      this.u[n] = gl.getUniformLocation(p, n);
    }
    this.tex = gl.createTexture()!;
    this.mask = gl.createTexture()!;
    gl.uniform1i(this.u.u_img, 0);
    gl.uniform1i(this.u.u_mask, 1);
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

  setMask(src: TexImageSource | null) {
    const gl = this.gl;
    gl.activeTexture(gl.TEXTURE1);
    gl.bindTexture(gl.TEXTURE_2D, this.mask);
    if (!src) {
      this.hasMask = false;
      gl.activeTexture(gl.TEXTURE0);
      return;
    }
    gl.pixelStorei(gl.UNPACK_COLORSPACE_CONVERSION_WEBGL, gl.NONE);
    gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA8, gl.RGBA, gl.UNSIGNED_BYTE, src);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
    gl.pixelStorei(gl.UNPACK_COLORSPACE_CONVERSION_WEBGL, gl.BROWSER_DEFAULT_WEBGL);
    gl.activeTexture(gl.TEXTURE0);
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

  draw(v: ViewParams) {
    const gl = this.gl;
    const u = this.u;
    gl.viewport(0, 0, this.canvas.width, this.canvas.height);
    gl.useProgram(this.prog);
    gl.activeTexture(gl.TEXTURE1);
    gl.bindTexture(gl.TEXTURE_2D, this.mask);
    gl.activeTexture(gl.TEXTURE0);
    gl.bindTexture(gl.TEXTURE_2D, this.tex);
    gl.uniform1i(u.u_hasImg, this.hasImg ? 1 : 0);
    gl.uniform1i(u.u_hasMask, this.hasMask ? 1 : 0);
    gl.uniform2f(u.u_canvas, this.canvas.width, this.canvas.height);
    gl.uniform2f(u.u_disp, v.dispW, v.dispH);
    gl.uniform2f(u.u_center, v.cx, v.cy);
    gl.uniform1f(u.u_scale, v.scale);
    gl.uniform1i(u.u_orient, v.orientation);
    gl.uniform2f(u.u_texel, 1 / Math.max(1, this.texW), 1 / Math.max(1, this.texH));
    gl.uniform1f(u.u_sharpen, v.sharpen);
    gl.uniform1i(u.u_peak, v.peaking);
    gl.uniform3fv(u.u_peakColor, v.peakColor);
    gl.uniform1i(u.u_over, v.over ? 1 : 0);
    gl.uniform1i(u.u_under, v.under ? 1 : 0);
    gl.uniform1i(u.u_overSolid, v.overSolid ? 1 : 0);
    gl.uniform3fv(u.u_underColor, v.underColor);
    gl.uniform3fv(u.u_bg, v.bg);
    gl.drawArrays(gl.TRIANGLE_STRIP, 0, 4);
  }

  destroy() {
    const gl = this.gl;
    gl.deleteTexture(this.tex);
    gl.deleteTexture(this.mask);
    gl.deleteProgram(this.prog);
    gl.getExtension('WEBGL_lose_context')?.loseContext();
  }
}
