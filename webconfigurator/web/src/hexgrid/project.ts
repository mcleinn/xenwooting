import { LTN_GRIDS, WTN_GRIDS, xyKey } from './boardGrids'

export type HexCoord = { x: number; y: number }

export type WtnVisHit = { board: 'Board0' | 'Board1'; visKey: number }

export function buildWtnCombinedLookup(): Map<string, WtnVisHit> {
  const m = new Map<string, WtnVisHit>()
  for (const b of ['Board0', 'Board1'] as const) {
    const g = WTN_GRIDS[b]
    for (const k of g.keys) {
      m.set(xyKey(k.x, k.y), { board: b, visKey: k.key })
    }
  }
  return m
}

export function ltnCoordFor(boardNum: number, key: number): HexCoord | null {
  const g =
    boardNum === 0
      ? LTN_GRIDS.Board0
      : boardNum === 1
        ? LTN_GRIDS.Board1
        : boardNum === 2
          ? LTN_GRIDS.Board2
          : boardNum === 3
            ? LTN_GRIDS.Board3
            : boardNum === 4
              ? LTN_GRIDS.Board4
              : null
  if (!g) return null
  const c = g.byKey.get(key)
  return c ? { x: c.x, y: c.y } : null
}

// --- 60-degree rotations on the doubled-y hex lattice ---
//
// Coordinate system used by boardGrids:
// neighbors are (0,+/-2), (+/-1,+/-1)
// We rotate using cube coordinates scaled by 2.

type Cube2 = { X: number; Y: number; Z: number }

function toCube2(p: HexCoord): Cube2 {
  // Axial: r = x, q = (y - x)/2. Use X=2q=(y-x), Z=2r=2x, Y=-X-Z.
  const X = p.y - p.x
  const Z = 2 * p.x
  const Y = -X - Z
  return { X, Y, Z }
}

function fromCube2(c: Cube2): HexCoord {
  const x = c.Z / 2
  const y = c.X + x
  return { x: x | 0, y: y | 0 }
}

function rot60Cube2(c: Cube2): Cube2 {
  // (x,y,z) -> (-z, -x, -y)
  return { X: -c.Z, Y: -c.X, Z: -c.Y }
}

export function rotateHex(p: HexCoord, steps60: number): HexCoord {
  let s = ((steps60 % 6) + 6) % 6
  let c = toCube2(p)
  while (s-- > 0) c = rot60Cube2(c)
  return fromCube2(c)
}

export function invRotateHex(p: HexCoord, steps60: number): HexCoord {
  return rotateHex(p, -steps60)
}

export function addHex(a: HexCoord, b: HexCoord): HexCoord {
  return { x: a.x + b.x, y: a.y + b.y }
}

export function subHex(a: HexCoord, b: HexCoord): HexCoord {
  return { x: a.x - b.x, y: a.y - b.y }
}
