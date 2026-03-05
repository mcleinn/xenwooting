export type GridCoord = { x: number; y: number }
export type GridKey = { key: number; x: number; y: number }

export type BoardGrid = {
  name: string
  keys: GridKey[]
  byKey: Map<number, GridCoord>
  byXY: Map<string, number>
}

export function buildBoardGrid(name: string, tuples: Array<[number, number, number]>): BoardGrid {
  const keys: GridKey[] = []
  const byKey = new Map<number, GridCoord>()
  const byXY = new Map<string, number>()

  for (const [key, x, y] of tuples) {
    keys.push({ key, x, y })
    byKey.set(key, { x, y })
    byXY.set(xyKey(x, y), key)
  }

  keys.sort((a, b) => a.key - b.key)
  return { name, keys, byKey, byXY }
}

export function xyKey(x: number, y: number) {
  return `${x},${y}`
}

// Neighbors for this doubled-y axial-like layout.
// - same row: y +/- 2
// - diagonals: x +/- 1 and y +/- 1
export const HEX_NEIGHBOR_DELTAS: Array<[dx: number, dy: number]> = [
  [0, -2],
  [0, 2],
  [-1, -1],
  [-1, 1],
  [1, -1],
  [1, 1],
]

export function neighborsOf(x: number, y: number): GridCoord[] {
  return HEX_NEIGHBOR_DELTAS.map(([dx, dy]) => ({ x: x + dx, y: y + dy }))
}

export type GridValidation = {
  ok: boolean
  errors: string[]
}

export function validateBoardGrid(grid: BoardGrid, expectedKeyCount: number, expectedKeys?: { min: number; max: number }): GridValidation {
  const errors: string[] = []

  // key uniqueness
  const keySeen = new Set<number>()
  const dupKeys: number[] = []
  for (const k of grid.keys) {
    if (keySeen.has(k.key)) dupKeys.push(k.key)
    keySeen.add(k.key)
  }
  if (dupKeys.length) errors.push(`duplicate keys: ${dupKeys.slice(0, 10).join(', ')}${dupKeys.length > 10 ? '…' : ''}`)

  // coord uniqueness
  const xySeen = new Set<string>()
  const dupXY: string[] = []
  for (const k of grid.keys) {
    const xy = xyKey(k.x, k.y)
    if (xySeen.has(xy)) dupXY.push(xy)
    xySeen.add(xy)
  }
  if (dupXY.length) errors.push(`duplicate (x,y): ${dupXY.slice(0, 10).join(' ')}${dupXY.length > 10 ? '…' : ''}`)

  if (grid.keys.length !== expectedKeyCount) {
    errors.push(`expected ${expectedKeyCount} keys, got ${grid.keys.length}`)
  }

  if (expectedKeys) {
    for (let k = expectedKeys.min; k <= expectedKeys.max; k++) {
      if (!grid.byKey.has(k)) errors.push(`missing key ${k}`)
    }
    for (const k of grid.keys) {
      if (k.key < expectedKeys.min || k.key > expectedKeys.max) errors.push(`unexpected key ${k.key}`)
    }
  }

  // connectivity
  if (grid.keys.length) {
    const start = grid.keys[0]
    const q: GridCoord[] = [{ x: start.x, y: start.y }]
    const seen = new Set<string>([xyKey(start.x, start.y)])
    while (q.length) {
      const cur = q.shift()!
      for (const nb of neighborsOf(cur.x, cur.y)) {
        const k = grid.byXY.get(xyKey(nb.x, nb.y))
        if (k === undefined) continue
        const id = xyKey(nb.x, nb.y)
        if (seen.has(id)) continue
        seen.add(id)
        q.push(nb)
      }
    }
    if (seen.size !== grid.keys.length) {
      errors.push(`grid disconnected: reached ${seen.size}/${grid.keys.length} cells by neighbor walk`)
    }
  }

  return { ok: errors.length === 0, errors }
}

export function validateGridSet(grids: Array<{ id: string; grid: BoardGrid }>): GridValidation {
  const errors: string[] = []

  // Overlap check: no two grids should share (x,y).
  const ownerByXY = new Map<string, string>()
  const overlaps: string[] = []
  for (const g of grids) {
    for (const k of g.grid.keys) {
      const xy = xyKey(k.x, k.y)
      const prev = ownerByXY.get(xy)
      if (prev && prev !== g.id) overlaps.push(`${xy} (${prev} & ${g.id})`)
      else ownerByXY.set(xy, g.id)
    }
  }
  if (overlaps.length) {
    errors.push(`overlap: ${overlaps.slice(0, 6).join(', ')}${overlaps.length > 6 ? '…' : ''}`)
  }

  // Connectivity check across the union: the set should form one connected component.
  const allXY = new Set<string>(ownerByXY.keys())
  if (allXY.size) {
    const start = allXY.values().next().value as string
    const q: string[] = [start]
    const seen = new Set<string>([start])
    while (q.length) {
      const cur = q.shift()!
      const [xs, ys] = cur.split(',')
      const x = Number.parseInt(xs || '', 10)
      const y = Number.parseInt(ys || '', 10)
      if (!Number.isFinite(x) || !Number.isFinite(y)) continue
      for (const [dx, dy] of HEX_NEIGHBOR_DELTAS) {
        const nb = xyKey(x + dx, y + dy)
        if (!allXY.has(nb) || seen.has(nb)) continue
        seen.add(nb)
        q.push(nb)
      }
    }
    if (seen.size !== allXY.size) {
      errors.push(`set disconnected: reached ${seen.size}/${allXY.size} cells by neighbor walk`)
    }
  }

  return { ok: errors.length === 0, errors }
}

// ---- Raw data (easy to edit) ----

export const WTN_BOARD0_TUPLES: Array<[number, number, number]> = [
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

export const WTN_BOARD1_TUPLES: Array<[number, number, number]> = [
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

export const LTN_BOARD0_TUPLES: Array<[number, number, number]> = [
  [0, 0, 0],
  [1, 0, 2],
  [2, 1, 1],
  [3, 1, 3],
  [4, 1, 5],
  [5, 1, 7],
  [6, 1, 9],
  [7, 2, 0],
  [8, 2, 2],
  [9, 2, 4],
  [10, 2, 6],
  [11, 2, 8],
  [12, 2, 10],
  [13, 3, 1],
  [14, 3, 3],
  [15, 3, 5],
  [16, 3, 7],
  [17, 3, 9],
  [18, 3, 11],
  [19, 4, 0],
  [20, 4, 2],
  [21, 4, 4],
  [22, 4, 6],
  [23, 4, 8],
  [24, 4, 10],
  [25, 5, 1],
  [26, 5, 3],
  [27, 5, 5],
  [28, 5, 7],
  [29, 5, 9],
  [30, 5, 11],
  [31, 6, 0],
  [32, 6, 2],
  [33, 6, 4],
  [34, 6, 6],
  [35, 6, 8],
  [36, 6, 10],
  [37, 7, 1],
  [38, 7, 3],
  [39, 7, 5],
  [40, 7, 7],
  [41, 7, 9],
  [42, 7, 11],
  [43, 8, 0],
  [44, 8, 2],
  [45, 8, 4],
  [46, 8, 6],
  [47, 8, 8],
  [48, 8, 10],
  [49, 9, 3],
  [50, 9, 5],
  [51, 9, 7],
  [52, 9, 9],
  [53, 9, 11],
  [54, 10, 8],
  [55, 10, 10],
]

export const LTN_BOARD1_TUPLES: Array<[number, number, number]> = [
  [0, 2, 12],
  [1, 2, 14],
  [2, 3, 13],
  [3, 3, 15],
  [4, 3, 17],
  [5, 3, 19],
  [6, 3, 21],
  [7, 4, 12],
  [8, 4, 14],
  [9, 4, 16],
  [10, 4, 18],
  [11, 4, 20],
  [12, 4, 22],
  [13, 5, 13],
  [14, 5, 15],
  [15, 5, 17],
  [16, 5, 19],
  [17, 5, 21],
  [18, 5, 23],
  [19, 6, 12],
  [20, 6, 14],
  [21, 6, 16],
  [22, 6, 18],
  [23, 6, 20],
  [24, 6, 22],
  [25, 7, 13],
  [26, 7, 15],
  [27, 7, 17],
  [28, 7, 19],
  [29, 7, 21],
  [30, 7, 23],
  [31, 8, 12],
  [32, 8, 14],
  [33, 8, 16],
  [34, 8, 18],
  [35, 8, 20],
  [36, 8, 22],
  [37, 9, 13],
  [38, 9, 15],
  [39, 9, 17],
  [40, 9, 19],
  [41, 9, 21],
  [42, 9, 23],
  [43, 10, 12],
  [44, 10, 14],
  [45, 10, 16],
  [46, 10, 18],
  [47, 10, 20],
  [48, 10, 22],
  [49, 11, 15],
  [50, 11, 17],
  [51, 11, 19],
  [52, 11, 21],
  [53, 11, 23],
  [54, 12, 20],
  [55, 12, 22],
]

export const LTN_BOARD2_TUPLES: Array<[number, number, number]> = [
  [0, 4, 24],
  [1, 4, 26],
  [2, 5, 25],
  [3, 5, 27],
  [4, 5, 29],
  [5, 5, 31],
  [6, 5, 33],
  [7, 6, 24],
  [8, 6, 26],
  [9, 6, 28],
  [10, 6, 30],
  [11, 6, 32],
  [12, 6, 34],
  [13, 7, 25],
  [14, 7, 27],
  [15, 7, 29],
  [16, 7, 31],
  [17, 7, 33],
  [18, 7, 35],
  [19, 8, 24],
  [20, 8, 26],
  [21, 8, 28],
  [22, 8, 30],
  [23, 8, 32],
  [24, 8, 34],
  [25, 9, 25],
  [26, 9, 27],
  [27, 9, 29],
  [28, 9, 31],
  [29, 9, 33],
  [30, 9, 35],
  [31, 10, 24],
  [32, 10, 26],
  [33, 10, 28],
  [34, 10, 30],
  [35, 10, 32],
  [36, 10, 34],
  [37, 11, 25],
  [38, 11, 27],
  [39, 11, 29],
  [40, 11, 31],
  [41, 11, 33],
  [42, 11, 35],
  [43, 12, 24],
  [44, 12, 26],
  [45, 12, 28],
  [46, 12, 30],
  [47, 12, 32],
  [48, 12, 34],
  [49, 13, 27],
  [50, 13, 29],
  [51, 13, 31],
  [52, 13, 33],
  [53, 13, 35],
  [54, 14, 32],
  [55, 14, 34],
]

export const LTN_BOARD3_TUPLES: Array<[number, number, number]> = [
  [0, 6, 36],
  [1, 6, 38],
  [2, 7, 37],
  [3, 7, 39],
  [4, 7, 41],
  [5, 7, 43],
  [6, 7, 45],
  [7, 8, 36],
  [8, 8, 38],
  [9, 8, 40],
  [10, 8, 42],
  [11, 8, 44],
  [12, 8, 46],
  [13, 9, 37],
  [14, 9, 39],
  [15, 9, 41],
  [16, 9, 43],
  [17, 9, 45],
  [18, 9, 47],
  [19, 10, 36],
  [20, 10, 38],
  [21, 10, 40],
  [22, 10, 42],
  [23, 10, 44],
  [24, 10, 46],
  [25, 11, 37],
  [26, 11, 39],
  [27, 11, 41],
  [28, 11, 43],
  [29, 11, 45],
  [30, 11, 47],
  [31, 12, 36],
  [32, 12, 38],
  [33, 12, 40],
  [34, 12, 42],
  [35, 12, 44],
  [36, 12, 46],
  [37, 13, 37],
  [38, 13, 39],
  [39, 13, 41],
  [40, 13, 43],
  [41, 13, 45],
  [42, 13, 47],
  [43, 14, 36],
  [44, 14, 38],
  [45, 14, 40],
  [46, 14, 42],
  [47, 14, 44],
  [48, 14, 46],
  [49, 15, 39],
  [50, 15, 41],
  [51, 15, 43],
  [52, 15, 45],
  [53, 15, 47],
  [54, 16, 44],
  [55, 16, 46],
]

export const LTN_BOARD4_TUPLES: Array<[number, number, number]> = [
  [0, 8, 48],
  [1, 8, 50],
  [2, 9, 49],
  [3, 9, 51],
  [4, 9, 53],
  [5, 9, 55],
  [6, 9, 57],
  [7, 10, 48],
  [8, 10, 50],
  [9, 10, 52],
  [10, 10, 54],
  [11, 10, 56],
  [12, 10, 58],
  [13, 11, 49],
  [14, 11, 51],
  [15, 11, 53],
  [16, 11, 55],
  [17, 11, 57],
  [18, 11, 59],
  [19, 12, 48],
  [20, 12, 50],
  [21, 12, 52],
  [22, 12, 54],
  [23, 12, 56],
  [24, 12, 58],
  [25, 13, 49],
  [26, 13, 51],
  [27, 13, 53],
  [28, 13, 55],
  [29, 13, 57],
  [30, 13, 59],
  [31, 14, 48],
  [32, 14, 50],
  [33, 14, 52],
  [34, 14, 54],
  [35, 14, 56],
  [36, 14, 58],
  [37, 15, 49],
  [38, 15, 51],
  [39, 15, 53],
  [40, 15, 55],
  [41, 15, 57],
  [42, 15, 59],
  [43, 16, 48],
  [44, 16, 50],
  [45, 16, 52],
  [46, 16, 54],
  [47, 16, 56],
  [48, 16, 58],
  [49, 17, 51],
  [50, 17, 53],
  [51, 17, 55],
  [52, 17, 57],
  [53, 17, 59],
  [54, 18, 56],
  [55, 18, 58],
]

export const WTN_GRIDS = {
  Board0: buildBoardGrid('wtn-board0', WTN_BOARD0_TUPLES),
  Board1: buildBoardGrid('wtn-board1', WTN_BOARD1_TUPLES),
}

export const LTN_GRIDS = {
  Board0: buildBoardGrid('ltn-board0', LTN_BOARD0_TUPLES),
  Board1: buildBoardGrid('ltn-board1', LTN_BOARD1_TUPLES),
  Board2: buildBoardGrid('ltn-board2', LTN_BOARD2_TUPLES),
  Board3: buildBoardGrid('ltn-board3', LTN_BOARD3_TUPLES),
  Board4: buildBoardGrid('ltn-board4', LTN_BOARD4_TUPLES),
}

export function validateAllGrids() {
  const wtnSet = validateGridSet([
    { id: 'WTN Board0', grid: WTN_GRIDS.Board0 },
    { id: 'WTN Board1', grid: WTN_GRIDS.Board1 },
  ])
  const ltnSet = validateGridSet([
    { id: 'LTN Board0', grid: LTN_GRIDS.Board0 },
    { id: 'LTN Board1', grid: LTN_GRIDS.Board1 },
    { id: 'LTN Board2', grid: LTN_GRIDS.Board2 },
    { id: 'LTN Board3', grid: LTN_GRIDS.Board3 },
    { id: 'LTN Board4', grid: LTN_GRIDS.Board4 },
  ])

  return {
    wtn: {
      Board0: validateBoardGrid(WTN_GRIDS.Board0, 53, { min: 0, max: 52 }),
      Board1: validateBoardGrid(WTN_GRIDS.Board1, 53, { min: 0, max: 52 }),
    },
    ltn: {
      Board0: validateBoardGrid(LTN_GRIDS.Board0, 56, { min: 0, max: 55 }),
      Board1: validateBoardGrid(LTN_GRIDS.Board1, 56, { min: 0, max: 55 }),
      Board2: validateBoardGrid(LTN_GRIDS.Board2, 56, { min: 0, max: 55 }),
      Board3: validateBoardGrid(LTN_GRIDS.Board3, 56, { min: 0, max: 55 }),
      Board4: validateBoardGrid(LTN_GRIDS.Board4, 56, { min: 0, max: 55 }),
    },
    sets: {
      wtn: wtnSet,
      ltn: ltnSet,
    },
  }
}
