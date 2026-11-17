"""Generates the Hyperfocal aperture icon.

    python3 assets/gen-icon.py
    rsvg-convert -w 512 assets/icon.svg -o assets/icon-512.png

Variants: icon.svg (rounded, transparent corners), icon-maskable.svg (full
bleed, PWA safe zone) and icon-square.svg (full bleed, for apple-touch-icon).
"""
import math, os
def aperture(cx, cy, R, r, rot_deg, colors, stroke=None):
    # hexagon opening vertices
    V=[(cx+r*math.cos(math.radians(rot_deg+60*i)), cy+r*math.sin(math.radians(rot_deg+60*i))) for i in range(6)]
    # line L_i: through V[i-1] -> V[i], extended beyond V[i] to circle
    P=[]
    for i in range(6):
        ax,ay=V[i-1]; bx,by=V[i]
        dx,dy=bx-ax,by-ay; n=math.hypot(dx,dy); dx/=n; dy/=n
        # solve |B + t d - C| = R, t>0
        fx,fy=bx-cx,by-cy
        b=fx*dx+fy*dy; c=fx*fx+fy*fy-R*R
        t=-b+math.sqrt(b*b-c)
        P.append((bx+t*dx, by+t*dy))
    paths=[]
    for i in range(6):
        vi=V[i]; pi=P[i]; pn=P[(i+1)%6]
        d=f"M{vi[0]:.2f},{vi[1]:.2f} L{pi[0]:.2f},{pi[1]:.2f} A{R},{R} 0 0 1 {pn[0]:.2f},{pn[1]:.2f} Z"
        paths.append(f'<path d="{d}" fill="{colors[i%len(colors)]}"/>')
    return "\n".join(paths), V

def svg(size=512, rounded=True, bleed=False):
    s=size; c=s/2
    R=s*0.30 if not bleed else s*0.26
    r=R*0.36
    blades,_=aperture(c,c,R,r,-90+30, ["url(#b0)","url(#b1)","url(#b2)","url(#b3)","url(#b4)","url(#b5)"])
    rx = s*0.225 if rounded else 0
    ring_w = s*0.022
    return f'''<svg xmlns="http://www.w3.org/2000/svg" width="{s}" height="{s}" viewBox="0 0 {s} {s}">
<defs>
  <linearGradient id="bg" x1="0" y1="0" x2="1" y2="1">
    <stop offset="0" stop-color="#1f3046"/><stop offset="0.55" stop-color="#141c25"/><stop offset="1" stop-color="#0d1217"/>
  </linearGradient>
  <radialGradient id="glow" cx="0.5" cy="0.5" r="0.5">
    <stop offset="0" stop-color="#4db1ff" stop-opacity="0.28"/><stop offset="1" stop-color="#4db1ff" stop-opacity="0"/>
  </radialGradient>
  <linearGradient id="b0" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="#8fd0ff"/><stop offset="1" stop-color="#4db1ff"/></linearGradient>
  <linearGradient id="b1" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="#5cbcff"/><stop offset="1" stop-color="#2a8ff5"/></linearGradient>
  <linearGradient id="b2" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="#3a9df8"/><stop offset="1" stop-color="#0a6fe0"/></linearGradient>
  <linearGradient id="b3" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="#1f7fe8"/><stop offset="1" stop-color="#0050b8"/></linearGradient>
  <linearGradient id="b4" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="#2d8cf0"/><stop offset="1" stop-color="#0a5fcc"/></linearGradient>
  <linearGradient id="b5" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="#6cc4ff"/><stop offset="1" stop-color="#3399f5"/></linearGradient>
  <linearGradient id="ring" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#cfe9ff" stop-opacity="0.9"/><stop offset="1" stop-color="#4db1ff" stop-opacity="0.35"/></linearGradient>
</defs>
<rect x="0" y="0" width="{s}" height="{s}" rx="{rx}" fill="url(#bg)"/>
<circle cx="{c}" cy="{c}" r="{R*1.55:.1f}" fill="url(#glow)"/>
<circle cx="{c}" cy="{c}" r="{R+ring_w*1.6:.1f}" fill="none" stroke="url(#ring)" stroke-width="{ring_w:.1f}"/>
<g stroke="#0d1217" stroke-width="{s*0.008:.2f}" stroke-linejoin="round">
{blades}
</g>
</svg>'''
if __name__ == '__main__':
    here = os.path.dirname(os.path.abspath(__file__))
    for name, args in (('icon.svg', (512, True)), ('icon-maskable.svg', (512, False, True)), ('icon-square.svg', (512, False, False))):
        with open(os.path.join(here, name), 'w') as f:
            f.write(svg(*args))
