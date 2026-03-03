import fs from 'node:fs/promises'

const BOARD_NAMES = ['Board0', 'Board1']
const CELL_COUNT = 56

export async function readWtnFile(filePath) {
  const text = await fs.readFile(filePath, 'utf8')
  return parseWtn(text)
}

export async function writeWtnFile(filePath, boards) {
  const text = formatWtn(boards)
  await fs.writeFile(filePath, text, 'utf8')
}

export function parseWtn(text) {
  const boards = {
    Board0: Array.from({ length: CELL_COUNT }, () => ({ note: 60, chan: 1, col: 'FFFFFF' })),
    Board1: Array.from({ length: CELL_COUNT }, () => ({ note: 60, chan: 1, col: 'FFFFFF' })),
  }

  /** @type {keyof typeof boards | null} */
  let section = null

  for (const rawLine of text.split(/\r?\n/)) {
    const line = rawLine.trim()
    if (!line) continue
    if (line.startsWith(';') || line.startsWith('#')) continue

    const mSection = line.match(/^\[(.+)]$/)
    if (mSection) {
      const name = mSection[1]
      section = BOARD_NAMES.includes(name) ? /** @type {any} */ (name) : null
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
    if (!Number.isFinite(idx) || idx < 0 || idx >= CELL_COUNT) continue

    if (kind === 'Key') {
      const note = Number.parseInt(v, 10)
      if (Number.isFinite(note)) boards[section][idx].note = note
    } else if (kind === 'Chan') {
      const chan = Number.parseInt(v, 10)
      if (Number.isFinite(chan)) boards[section][idx].chan = chan
    } else if (kind === 'Col') {
      const col = v.replace(/^#/, '').toUpperCase()
      if (/^[0-9A-F]{6}$/.test(col)) boards[section][idx].col = col
    }
  }

  return boards
}

export function formatWtn(boards) {
  const parts = []
  for (const boardName of BOARD_NAMES) {
    const cells = boards[boardName]
    parts.push(`[${boardName}]`)
    for (let i = 0; i < CELL_COUNT; i++) {
      const cell = cells?.[i] || { note: 60, chan: 1, col: 'FFFFFF' }
      const note = clampInt(cell.note, 0, 127)
      const chan = clampInt(cell.chan, 1, 16)
      const col = normalizeHex6(cell.col)
      parts.push(`Key_${i}=${note}`)
      parts.push(`Chan_${i}=${chan}`)
      parts.push(`Col_${i}=${col}`)
    }
    parts.push('')
  }
  return parts.join('\n')
}

function clampInt(v, min, max) {
  const n = Number(v)
  if (!Number.isFinite(n)) return min
  const i = Math.trunc(n)
  return Math.min(max, Math.max(min, i))
}

function normalizeHex6(v) {
  if (typeof v !== 'string') return 'FFFFFF'
  const col = v.replace(/^#/, '').toUpperCase()
  if (/^[0-9A-F]{6}$/.test(col)) return col
  return 'FFFFFF'
}
