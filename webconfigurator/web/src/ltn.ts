export type LtnCell = {
  note: number
  chan: number
  col: string // RRGGBB
}

export type LtnBoard = {
  // index 0..55
  cells: LtnCell[]
}

export type LtnFile = {
  // boards in file order: Board0..BoardN
  boards: LtnBoard[]
}

// Lumatone base pattern (56 keys) from xenAssist (hex grid).
// Values are key numbers 1..56, 0 means no key.
const LUM_PATTERN: number[][] = [
  [1, 2, 0, 0, 0, 0],
  [3, 4, 5, 6, 7, 0],
  [8, 9, 10, 11, 12, 13],
  [14, 15, 16, 17, 18, 19],
  [20, 21, 22, 23, 24, 25],
  [26, 27, 28, 29, 30, 31],
  [32, 33, 34, 35, 36, 37],
  [38, 39, 40, 41, 42, 43],
  [44, 45, 46, 47, 48, 49],
  [0, 50, 51, 52, 53, 54],
  [0, 0, 0, 0, 55, 56],
]

export type LtnPlacedKey = {
  // half-U coordinates
  x2: number
  // row units
  y: number
  cell: LtnCell
}

export function parseLtnText(text: string): LtnFile {
  const boards: Record<string, LtnCell[]> = {}
  let section: string | null = null

  for (const rawLine of text.split(/\r?\n/)) {
    const line = rawLine.trim()
    if (!line) continue
    if (line.startsWith(';') || line.startsWith('#')) continue

    const mSection = line.match(/^\[(.+)]$/)
    if (mSection) {
      const name = mSection[1]
      section = /^Board\d+$/.test(name) ? name : null
      if (section && !boards[section]) {
        boards[section] = Array.from({ length: 56 }, () => ({ note: 60, chan: 1, col: 'FFFFFF' }))
      }
      continue
    }

    if (!section) continue
    const eq = line.indexOf('=')
    if (eq === -1) continue
    const k = line.slice(0, eq).trim()
    const v = line.slice(eq + 1).trim()

    const mKey = k.match(/^(Key|Chan|Col)_(\d+)$/)
    if (!mKey) continue
    const kind = mKey[1]
    const idx = Number.parseInt(mKey[2], 10)
    if (!Number.isFinite(idx) || idx < 0 || idx >= 56) continue

    const cell = boards[section][idx]
    if (kind === 'Key') {
      const note = Number.parseInt(v, 10)
      if (Number.isFinite(note)) cell.note = clampInt(note, 0, 127)
    } else if (kind === 'Chan') {
      const chan = Number.parseInt(v, 10)
      if (Number.isFinite(chan)) cell.chan = clampInt(chan, 1, 16)
    } else if (kind === 'Col') {
      const col = normalizeHex6(v)
      if (col) cell.col = col
    }
  }

  const boardNames = Object.keys(boards)
    .filter((k) => /^Board\d+$/.test(k))
    .sort((a, b) => Number.parseInt(a.slice(5), 10) - Number.parseInt(b.slice(5), 10))

  return {
    boards: boardNames.map((bn) => ({ cells: boards[bn] })),
  }
}

export function placeLtnKeys(ltn: LtnFile): LtnPlacedKey[] {
  // Map key index -> (row, col) within base pattern.
  const rcByIdx = new Map<number, { r: number; c: number }>()
  for (let r = 0; r < LUM_PATTERN.length; r++) {
    for (let c = 0; c < LUM_PATTERN[r].length; c++) {
      const v = LUM_PATTERN[r][c]
      if (v > 0) rcByIdx.set(v - 1, { r, c })
    }
  }

  const out: LtnPlacedKey[] = []
  for (let b = 0; b < ltn.boards.length; b++) {
    const board = ltn.boards[b]
    for (let i = 0; i < 56; i++) {
      const rc = rcByIdx.get(i)
      if (!rc) continue
      const r = rc.r
      const c = rc.c

      // Base pattern coordinates in half-U.
      const baseX2 = 2 * c + (r % 2 === 0 ? 1 : 0)
      const baseY = r

      // Repeat placement like xenAssist:
      // - shift right by patternCols*rep + 1
      // - shift down by 2 rows per repetition
      const x2 = baseX2 + 2 * (b * 6 + 1)
      const y = baseY + b * 2

      out.push({ x2, y, cell: board.cells[i] })
    }
  }
  return out
}

function clampInt(n: number, min: number, max: number) {
  const i = Math.trunc(n)
  return Math.min(max, Math.max(min, i))
}

function normalizeHex6(v: string) {
  const s = v.trim().replace(/^#/, '').toUpperCase()
  const m = s.match(/([0-9A-F]{6})$/)
  if (!m) return null
  return m[1]
}
