// Computes isomorphic axis step sizes (dq, dr) for a layout.
//
// This is intentionally server-side so /wtn/guide can remain a pure
// isomorphic pattern-space view (no per-board/invalid-key logic in the UI).

function xyKey(x, y) {
  return `${x},${y}`
}

function mod(n, m) {
  const x = n % m
  return x < 0 ? x + m : x
}

// Neighbors for this doubled-y axial-like layout.
// - same row: y +/- 2
// - diagonals: x +/- 1 and y +/- 1
const HEX_NEIGHBOR_DELTAS = [
  [0, -2],
  [0, 2],
  [-1, -1],
  [-1, 1],
  [1, -1],
  [1, 1],
]

function buildBoardGrid(tuples) {
  const byKey = new Map()
  const byXY = new Map()
  for (const [key, x, y] of tuples) {
    byKey.set(key, { x, y })
    byXY.set(xyKey(x, y), key)
  }
  return { byKey, byXY }
}

// Copied from web/src/hexgrid/boardGrids.ts (WTN only). Key ids are 0..52.
const WTN_BOARD0_TUPLES = [
  [0, 0, 1],
  [1, 0, 3],
  [2, 0, 5],
  [3, 0, 7],
  [4, 0, 9],
  [5, 0, 11],
  [6, 0, 13],
  [7, 0, 15],
  [8, 0, 17],
  [9, 0, 19],
  [10, 0, 21],
  [11, 0, 23],
  [12, 1, 0],
  [13, 1, 2],
  [14, 1, 4],
  [15, 1, 6],
  [16, 1, 8],
  [17, 1, 10],
  [18, 1, 12],
  [19, 1, 14],
  [20, 1, 16],
  [21, 1, 18],
  [22, 1, 20],
  [23, 1, 22],
  [24, 1, 24],
  [25, 2, -1],
  [26, 2, 1],
  [27, 2, 3],
  [28, 2, 5],
  [29, 2, 7],
  [30, 2, 9],
  [31, 2, 11],
  [32, 2, 13],
  [33, 2, 15],
  [34, 2, 17],
  [35, 2, 19],
  [36, 2, 21],
  [37, 2, 23],
  [38, 2, 25],
  [39, 3, 0],
  [40, 3, 2],
  [41, 3, 4],
  [42, 3, 6],
  [43, 3, 8],
  [44, 3, 10],
  [45, 3, 12],
  [46, 3, 14],
  [47, 3, 16],
  [48, 3, 18],
  [49, 3, 20],
  [50, 3, 22],
  [51, 3, 24],
  [52, 3, 26],
]

const WTN_BOARD1_TUPLES = [
  [0, 4, -5],
  [1, 4, -3],
  [2, 4, -1],
  [3, 4, 1],
  [4, 4, 3],
  [5, 4, 5],
  [6, 4, 7],
  [7, 4, 9],
  [8, 4, 11],
  [9, 4, 13],
  [10, 4, 15],
  [11, 4, 17],
  [12, 4, 19],
  [13, 4, 21],
  [14, 5, -4],
  [15, 5, -2],
  [16, 5, 0],
  [17, 5, 2],
  [18, 5, 4],
  [19, 5, 6],
  [20, 5, 8],
  [21, 5, 10],
  [22, 5, 12],
  [23, 5, 14],
  [24, 5, 16],
  [25, 5, 18],
  [26, 5, 20],
  [27, 5, 22],
  [28, 6, -3],
  [29, 6, -1],
  [30, 6, 1],
  [31, 6, 3],
  [32, 6, 5],
  [33, 6, 7],
  [34, 6, 9],
  [35, 6, 11],
  [36, 6, 13],
  [37, 6, 15],
  [38, 6, 17],
  [39, 6, 19],
  [40, 6, 21],
  [41, 7, -2],
  [42, 7, 0],
  [43, 7, 2],
  [44, 7, 4],
  [45, 7, 6],
  [46, 7, 8],
  [47, 7, 10],
  [48, 7, 12],
  [49, 7, 14],
  [50, 7, 16],
  [51, 7, 18],
  [52, 7, 20],
]

const WTN_GRIDS = {
  Board0: buildBoardGrid(WTN_BOARD0_TUPLES),
  Board1: buildBoardGrid(WTN_BOARD1_TUPLES),
}

function clampInt(n, min, max) {
  const x = Number.isFinite(n) ? Math.trunc(n) : min
  return Math.min(max, Math.max(min, x))
}

// Equivalent to App.tsx buildVisibleIndexMap().
function buildVisibleIndexMap(geometryKeys, rotate180) {
  const keys = Array.isArray(geometryKeys) ? geometryKeys : []
  const minColByRow = (() => {
    if (!rotate180) return [0, 0, 0, 0]
    const min = [255, 255, 255, 255]
    for (const k of keys) {
      const rr = 3 - (k?.row ?? 0)
      const cc = 13 - (k?.col ?? 0)
      min[rr] = Math.min(min[rr], cc)
    }
    for (let i = 0; i < 4; i++) if (min[i] === 255) min[i] = 0
    return min
  })()

  const tmp = []
  for (const k of keys) {
    const row = k?.row
    const col = k?.col
    if (!Number.isInteger(row) || !Number.isInteger(col)) continue
    const rr = rotate180 ? 3 - row : row
    const cc0 = rotate180 ? 13 - col : col
    const cc = cc0 - (minColByRow[rr] || 0)
    const idx = rr * 14 + cc
    tmp.push({ rr, cc, idx })
  }
  tmp.sort((a, b) => (a.rr - b.rr) * 100 + (a.cc - b.cc))
  return tmp.map((t) => t.idx)
}

function stepForDir(ptsByXY, dx, dy, edo) {
  const counts = new Map()
  let edges = 0
  for (const [xy, from] of ptsByXY.entries()) {
    const [xs, ys] = String(xy).split(',')
    const x = Number.parseInt(xs || '', 10)
    const y = Number.parseInt(ys || '', 10)
    if (!Number.isFinite(x) || !Number.isFinite(y)) continue
    const to = ptsByXY.get(xyKey(x + dx, y + dy))
    if (!to) continue
    edges++
    // For /wtn/guide we only need pitch-class linearity.
    // Channel changes can add multiples of `edo` to absolute pitch deltas, so compute modulo.
    const dp = mod((to.pitch - from.pitch) | 0, edo)
    counts.set(dp, (counts.get(dp) || 0) + 1)
  }
  return { edges, counts }
}

function pickUnique(counts) {
  if (!counts || counts.size !== 1) return { ok: false, value: null }
  const it = counts.keys().next()
  return { ok: true, value: it.value }
}

function computeBoardAxisSteps({ geometryKeys, boards, boardName, rotate180, edo, pitchOffset }) {
  const internalByVis = buildVisibleIndexMap(geometryKeys, rotate180)
  const grid = WTN_GRIDS[boardName]
  const ptsByXY = new Map()

  for (let visKey = 0; visKey < internalByVis.length; visKey++) {
    const internalIdx = internalByVis[visKey]
    const coord = grid.byKey.get(visKey)
    const cell = boards?.[boardName]?.[internalIdx]
    if (!coord || !cell) continue
    const chan0 = clampInt((cell.chan | 0) - 1, 0, 15)
    const note = clampInt(cell.note | 0, 0, 127)
    const pitch = (chan0 * edo + note + (pitchOffset | 0)) | 0
    ptsByXY.set(xyKey(coord.x, coord.y), { x: coord.x, y: coord.y, pitch })
  }

  // Basis directions for the linear PC model:
  // - dq: (dx=0, dy=+2) i.e. q+1
  // - dr: (dx=+1, dy=+1) i.e. r+1
  const dQ = stepForDir(ptsByXY, 0, 2, edo)
  const dR = stepForDir(ptsByXY, 1, 1, edo)

  const uq = pickUnique(dQ.counts)
  const ur = pickUnique(dR.counts)

  const reasons = []
  if (dQ.edges === 0) reasons.push('no neighbor edges found for dq direction')
  if (dR.edges === 0) reasons.push('no neighbor edges found for dr direction')
  if (!uq.ok && dQ.edges) reasons.push('dq not constant across the grid')
  if (!ur.ok && dR.edges) reasons.push('dr not constant across the grid')

  // Optional consistency check: if we have any edges for the 3rd axis direction,
  // it should equal (dr - dq) mod edo.
  const d3 = stepForDir(ptsByXY, 1, -1, edo)
  const u3 = pickUnique(d3.counts)
  if (d3.edges && !u3.ok) reasons.push('3rd axis step not constant across the grid')
  if (uq.ok && ur.ok && u3.ok) {
    const expected = mod((ur.value - uq.value) | 0, edo)
    if (u3.value !== expected) reasons.push('3rd axis step does not match dr - dq')
  }

  return {
    ok: reasons.length === 0,
    dq: uq.ok ? uq.value : null,
    dr: ur.ok ? ur.value : null,
    axis3: uq.ok && ur.ok ? mod((ur.value - uq.value) | 0, edo) : null,
    details: {
      dq: { edges: dQ.edges, values: Array.from(dQ.counts.keys()).sort((a, b) => a - b) },
      dr: { edges: dR.edges, values: Array.from(dR.counts.keys()).sort((a, b) => a - b) },
      axis3: { edges: d3.edges, values: Array.from(d3.counts.keys()).sort((a, b) => a - b) },
      reasons,
    },
  }
}

export function computeSharedIsomorphicAxisSteps({ geometryKeys, boards, edo, pitchOffset }) {
  const e = Number.parseInt(String(edo ?? ''), 10)
  if (!Number.isInteger(e) || e < 1 || e > 999) {
    return { ok: false, reason: 'invalid edo', edo: e }
  }

  const off = Number.parseInt(String(pitchOffset ?? 0), 10) | 0
  const b0 = computeBoardAxisSteps({ geometryKeys, boards, boardName: 'Board0', rotate180: true, edo: e, pitchOffset: off })
  const b1 = computeBoardAxisSteps({ geometryKeys, boards, boardName: 'Board1', rotate180: false, edo: e, pitchOffset: off })

  if (!b0.ok || !b1.ok) {
    return {
      ok: false,
      edo: e,
      reason: 'not isomorphic',
      Board0: b0,
      Board1: b1,
    }
  }

  if (b0.dq !== b1.dq || b0.dr !== b1.dr) {
    return {
      ok: false,
      edo: e,
      reason: 'Board0 and Board1 axis steps differ',
      Board0: b0,
      Board1: b1,
    }
  }

  const dq = b0.dq | 0
  const dr = b0.dr | 0
  const axis3 = mod((dr - dq) | 0, e)

  return {
    ok: true,
    edo: e,
    dq,
    dr,
    axis3,
    Board0: b0,
    Board1: b1,
  }
}
